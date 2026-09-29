//! Per-`equiv` on-disk cache for `kio test`.
//!
//! The cache stores the observable discharge result for one typed
//! `equiv` declaration: whether it passed and the exact stdout block
//! `kio test` would have printed after a fresh partial-evaluation run.
//! Hits therefore skip normalization work without changing output.
//!
//! ## Storage layout
//!
//! ```text
//! <cache>/equiv/
//!   <hex>.bin
//!   <hex>.bin.tmp.*
//! ```
//!
//! The key is BLAKE3 over the source declaration identity, its
//! substituted Kio' terms, the recursively referenced Kio' function
//! definitions the evaluator may unfold, the body-free package structure,
//! the explicit primitive environment, the newtype registry the evaluator's
//! ι-rules consult, and the compiler/cache identity fields. Malformed or
//! stale entries, exact-key mismatches, and corrupt payloads are misses;
//! loaders never panic.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use blake3::Hasher;

use crate::ast::{
    CallArg, Expr, FnDef, ImportKind, Item, Module, ModulePath, Signature, SignatureParam,
    UncheckedPrime,
};
use crate::cache::identity::{
    COMPILER_CACHE_ID, COMPILER_VERSION, FEATURE_SET, IMPLEMENTATION_TAG,
};
use crate::cache::keys::{DeclaredModulePath, EquivRenderMode};
use crate::pass::resolve::Package;
use crate::path_display::DisplayPath;

pub const CACHE_NAMESPACE: &str = "equiv";

static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct EquivCache {
    inner: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    Active { cache_root: PathBuf, root: PathBuf },
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EquivCacheEntry {
    pub passed: bool,
    pub output: String,
}

pub struct EquivCacheKeyInput<'a> {
    pub prelude: &'a EquivCacheKeyPrelude,
    pub module_path: &'a DeclaredModulePath,
    pub name: &'a str,
    pub sig: &'a Signature<UncheckedPrime>,
    pub terms: &'a [Expr<UncheckedPrime>],
    pub package: &'a Package<UncheckedPrime>,
    pub module: &'a Module<UncheckedPrime>,
}

/// The package-global half of every [`EquivCacheKey`] in one `kio test`
/// run, hashed **once** into a fixed-size digest.
///
/// The body-free package structure, primitive environment, and newtype
/// registry are identical for every `equiv` in the package. Hashing those
/// shared inputs once, before the parallel fan-out, avoids repeating that
/// work. Substituted terms and their referenced function bodies remain in
/// each per-`equiv` key so an unrelated function-body edit cannot invalidate
/// every cache entry in the package.
///
/// [`EquivCacheKey::new`] consumes only the 32-byte digest, so a
/// per-`equiv` key cannot reach the shared inputs to re-hash them: the
/// repeated-work regression is unrepresentable, not merely avoided.
#[derive(Debug, Clone)]
pub struct EquivCacheKeyPrelude {
    digest: [u8; blake3::OUT_LEN],
}

impl EquivCacheKeyPrelude {
    /// Hash every [`EquivCacheKey`] input shared by the `equiv` blocks of one
    /// package run. Ordinary function bodies are the sole excluded package
    /// input; each per-`equiv` key hashes only the bodies reachable from its
    /// terms.
    pub fn new(
        render_mode: &EquivRenderMode,
        package: &Package<UncheckedPrime>,
        primitives: &crate::normalization::EvalPrimitiveEnv,
        newtype_registry: &crate::normalization::NewtypeRegistry,
    ) -> Self {
        let package_inputs = EvalPackageSharedKeyInputs::from_package(package);
        let primitive_inputs = EvalPrimitiveKeyInputs::from_env(primitives);
        let newtype_inputs = NewtypeRegistryKeyInputs::from_registry(newtype_registry);
        let mut h = Hasher::new();
        write_framed(&mut h, CACHE_NAMESPACE.as_bytes());
        write_framed(&mut h, IMPLEMENTATION_TAG.as_bytes());
        write_framed(&mut h, COMPILER_CACHE_ID.as_bytes());
        write_framed(&mut h, render_mode.as_str().as_bytes());
        write_stable_ast_framed(&mut h, &package_inputs);
        write_stable_ast_framed(&mut h, &primitive_inputs);
        write_stable_ast_framed(&mut h, &newtype_inputs);
        EquivCacheKeyPrelude {
            digest: *h.finalize().as_bytes(),
        }
    }
}

#[derive(serde::Serialize)]
struct EvalPackageSharedKeyInputs<'a> {
    package_file: Option<EvalPackageFileKeyInput<'a>>,
    modules: Vec<EvalModuleSharedKeyInput<'a>>,
}

#[derive(serde::Serialize)]
struct EvalPackageFileKeyInput<'a> {
    package_name: &'a str,
    package_file: &'a crate::ast::PackageFile<UncheckedPrime>,
}

#[derive(serde::Serialize)]
struct EvalModuleSharedKeyInput<'a> {
    canonical_path: &'a str,
    declared_path: &'a ModulePath,
    imports: &'a [crate::ast::Import],
    items: Vec<EvalItemSharedKeyInput<'a>>,
}

#[derive(serde::Serialize)]
enum EvalItemSharedKeyInput<'a> {
    Fn(EvalFnHeaderKeyInput<'a>),
    Other(&'a Item<UncheckedPrime>),
}

struct EvalFnHeaderKeyInput<'a>(&'a FnDef<UncheckedPrime>);

impl serde::Serialize for EvalFnHeaderKeyInput<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let FnDef {
            vis,
            purity,
            name,
            sig,
            ret,
            ret_elided,
            body: _,
            meta,
            doc,
        } = self.0;
        let mut header = serializer.serialize_struct("FnDefHeader", 8)?;
        header.serialize_field("vis", vis)?;
        header.serialize_field("purity", purity)?;
        header.serialize_field("name", name)?;
        header.serialize_field("sig", sig)?;
        header.serialize_field("ret", ret)?;
        header.serialize_field("ret_elided", ret_elided)?;
        header.serialize_field("meta", meta)?;
        header.serialize_field("doc", doc)?;
        header.end()
    }
}

impl<'a> EvalPackageSharedKeyInputs<'a> {
    fn from_package(package: &'a Package<UncheckedPrime>) -> Self {
        Self {
            package_file: package.package_file().map(|entry| EvalPackageFileKeyInput {
                package_name: &entry.package_name,
                package_file: &entry.package_file,
            }),
            modules: package
                .modules()
                .map(|(canonical_path, entry)| EvalModuleSharedKeyInput {
                    canonical_path,
                    declared_path: &entry.module.path,
                    imports: &entry.module.imports,
                    items: entry
                        .module
                        .items
                        .iter()
                        .map(|item| match item {
                            Item::FnDef(def) => {
                                EvalItemSharedKeyInput::Fn(EvalFnHeaderKeyInput(def))
                            }
                            other => EvalItemSharedKeyInput::Other(other),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(serde::Serialize)]
struct EvalPrimitiveKeyInputs {
    modules: Vec<(String, Vec<(String, crate::comptime::ComptimeBuiltin)>)>,
}

impl EvalPrimitiveKeyInputs {
    fn from_env(env: &crate::normalization::EvalPrimitiveEnv) -> Self {
        Self {
            modules: env
                .bindings()
                .map(|(module, bindings)| {
                    (
                        module.to_owned(),
                        bindings
                            .map(|(name, builtin)| (name.to_owned(), builtin))
                            .collect(),
                    )
                })
                .collect(),
        }
    }
}

impl EquivCache {
    pub fn open(cache_root: PathBuf) -> std::io::Result<Self> {
        let dir = cache_root.join("equiv");
        fs::create_dir_all(&dir)?;
        write_gitignore_if_absent(&cache_root)?;
        crate::package_collection::mark_generated_dir(&cache_root);
        crate::cache::gc::record_cache_open(&cache_root, crate::cache::gc::CacheFamily::Equiv);
        Ok(EquivCache {
            inner: Backend::Active {
                cache_root,
                root: dir,
            },
        })
    }

    pub fn disabled() -> Self {
        EquivCache {
            inner: Backend::Disabled,
        }
    }

    /// Whether the cache is backed by storage. Callers use this to skip
    /// building an [`EquivCacheKeyPrelude`] when no result can be loaded or
    /// stored.
    pub fn is_active(&self) -> bool {
        matches!(self.inner, Backend::Active { .. })
    }

    pub fn key_for(&self, input: EquivCacheKeyInput<'_>) -> Option<EquivCacheKey> {
        let Backend::Active { .. } = &self.inner else {
            return None;
        };
        Some(EquivCacheKey::new(
            input.prelude,
            input.module_path,
            input.name,
            input.sig,
            input.terms,
            input.package,
            input.module,
        ))
    }

    pub fn lookup(
        &self,
        key: &EquivCacheKey,
        module_path: &str,
        equiv_name: &str,
    ) -> Option<EquivCacheEntry> {
        match &self.inner {
            Backend::Disabled => {
                log_probe("miss", key, module_path, equiv_name, "cache disabled");
                None
            }
            Backend::Active { cache_root, root } => match read_entry(root, key) {
                Ok(Some(entry)) => {
                    crate::cache::gc::record_entry_path_access(
                        cache_root,
                        crate::cache::gc::CacheFamily::Equiv,
                        root,
                        &entry_path(root, key),
                    );
                    log_probe("hit", key, module_path, equiv_name, "");
                    Some(entry)
                }
                Ok(None) => {
                    log_probe("miss", key, module_path, equiv_name, "no entry");
                    None
                }
                Err(reason) => {
                    log_probe("miss", key, module_path, equiv_name, &reason);
                    None
                }
            },
        }
    }

    pub fn store(
        &self,
        key: &EquivCacheKey,
        module_path: &str,
        equiv_name: &str,
        entry: &EquivCacheEntry,
    ) {
        let Backend::Active { cache_root, root } = &self.inner else {
            return;
        };
        if let Err(reason) = write_entry(root, key, entry) {
            log_probe("store-fail", key, module_path, equiv_name, &reason);
        } else {
            crate::cache::gc::record_entry_path_access(
                cache_root,
                crate::cache::gc::CacheFamily::Equiv,
                root,
                &entry_path(root, key),
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EquivCacheKey {
    hex: String,
}

impl EquivCacheKey {
    pub fn new(
        prelude: &EquivCacheKeyPrelude,
        module_path: &DeclaredModulePath,
        name: &str,
        sig: &Signature<UncheckedPrime>,
        terms: &[Expr<UncheckedPrime>],
        package: &Package<UncheckedPrime>,
        module: &Module<UncheckedPrime>,
    ) -> Self {
        let closure = referenced_definition_closure(sig, terms, package, module);
        let identity = EvalEquivIdentity { name, sig };
        let mut h = Hasher::new();
        write_framed(&mut h, &prelude.digest);
        write_framed(&mut h, module_path.as_str().as_bytes());
        write_stable_ast_framed(&mut h, &identity);
        write_stable_ast_framed(&mut h, &terms);
        write_stable_ast_framed(&mut h, &closure);
        EquivCacheKey {
            hex: h.finalize().to_hex().to_string(),
        }
    }

    pub fn hex(&self) -> &str {
        &self.hex
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
enum FnIdentity {
    Canonical { module_path: String, name: String },
}

#[derive(Debug, Clone, serde::Serialize)]
struct ClosureEntry<'a> {
    identity: FnIdentity,
    def: &'a FnDef<UncheckedPrime>,
}

#[derive(serde::Serialize)]
struct EvalEquivIdentity<'a> {
    name: &'a str,
    sig: &'a Signature<UncheckedPrime>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
struct EquivProjectorName(String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(transparent)]
struct EquivConstructorName(String);

#[derive(Debug, Clone, serde::Serialize)]
struct NewtypeRegistryKeyInputs {
    proj_ctor: Vec<(EquivProjectorName, EquivConstructorName)>,
    ctors: BTreeSet<EquivConstructorName>,
    ctor_canonical: Vec<(EquivConstructorName, EquivConstructorName)>,
    ctor_abi_arity: Vec<(EquivConstructorName, usize)>,
}

impl NewtypeRegistryKeyInputs {
    fn from_registry(registry: &crate::normalization::NewtypeRegistry) -> Self {
        let mut proj_ctor: Vec<_> = registry
            .proj_ctor
            .iter()
            .map(|(k, v)| {
                (
                    EquivProjectorName(k.clone()),
                    EquivConstructorName(v.clone()),
                )
            })
            .collect();
        proj_ctor.sort();

        let mut ctor_canonical: Vec<_> = registry
            .ctor_canonical
            .iter()
            .map(|(k, v)| {
                (
                    EquivConstructorName(k.clone()),
                    EquivConstructorName(v.clone()),
                )
            })
            .collect();
        ctor_canonical.sort();

        let mut ctor_abi_arity: Vec<_> = registry
            .ctor_abi_arity
            .iter()
            .map(|(k, v)| (EquivConstructorName(k.clone()), *v))
            .collect();
        ctor_abi_arity.sort();

        NewtypeRegistryKeyInputs {
            proj_ctor,
            ctors: registry
                .ctors
                .iter()
                .cloned()
                .map(EquivConstructorName)
                .collect(),
            ctor_canonical,
            ctor_abi_arity,
        }
    }
}

fn referenced_definition_closure<'a>(
    sig: &Signature<UncheckedPrime>,
    terms: &[Expr<UncheckedPrime>],
    package: &'a Package<UncheckedPrime>,
    module: &'a Module<UncheckedPrime>,
) -> Vec<ClosureEntry<'a>> {
    let mut bound = value_params(sig);
    let mut pending = BTreeSet::new();
    let module_path = package
        .modules()
        .find_map(|(path, entry)| std::ptr::eq(&entry.module, module).then_some(path))
        .map(str::to_owned)
        .unwrap_or_else(|| module_import_path(&module.path));
    for term in terms {
        collect_expr_refs(
            term,
            &mut bound,
            package,
            module,
            &module_path,
            &mut pending,
        );
    }

    let mut seen = BTreeSet::new();
    let mut entries: BTreeMap<FnIdentity, &'a FnDef<UncheckedPrime>> = BTreeMap::new();
    while let Some(identity) = pending.pop_first() {
        if !seen.insert(identity.clone()) {
            continue;
        }
        let Some((def, owner)) = lookup_fn(&identity, package) else {
            continue;
        };
        entries.insert(identity.clone(), def);
        let mut def_bound = value_params(&def.sig);
        let FnIdentity::Canonical { module_path, .. } = &identity;
        collect_expr_refs(
            &def.body,
            &mut def_bound,
            package,
            owner,
            module_path,
            &mut pending,
        );
    }

    entries
        .into_iter()
        .map(|(identity, def)| ClosureEntry { identity, def })
        .collect()
}

fn lookup_fn<'a>(
    identity: &FnIdentity,
    package: &'a Package<UncheckedPrime>,
) -> Option<(&'a FnDef<UncheckedPrime>, &'a Module<UncheckedPrime>)> {
    let FnIdentity::Canonical { module_path, name } = identity;
    let owner = &package.module(module_path)?.module;
    find_module_fn(owner, name).map(|def| (def, owner))
}

fn value_params<P: crate::ast::Phase>(sig: &crate::ast::Signature<P>) -> BTreeSet<String> {
    sig.params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(vp) => Some(vp.name.clone()),
            SignatureParam::Type(_) => None,
        })
        .collect()
}

fn collect_expr_refs(
    expr: &Expr<UncheckedPrime>,
    bound: &mut BTreeSet<String>,
    package: &Package<UncheckedPrime>,
    module: &Module<UncheckedPrime>,
    module_path: &str,
    out: &mut BTreeSet<FnIdentity>,
) {
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => {
            collect_path_refs(segments, bound, package, module, module_path, out)
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_refs(callee, bound, package, module, module_path, out);
            for arg in args {
                if let CallArg::Value(v) = arg {
                    collect_expr_refs(v, bound, package, module, module_path, out);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            let added = push_value_params(bound, sig);
            collect_expr_refs(body, bound, package, module, module_path, out);
            pop_names(bound, added);
        }
        Expr::Let {
            name, value, body, ..
        } => {
            collect_expr_refs(value, bound, package, module, module_path, out);
            let inserted = bound.insert(name.clone());
            collect_expr_refs(body, bound, package, module, module_path, out);
            if inserted {
                bound.remove(name);
            }
        }
        Expr::Seq { value, body, .. } => {
            collect_expr_refs(value, bound, package, module, module_path, out);
            collect_expr_refs(body, bound, package, module, module_path, out);
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn collect_path_refs(
    segments: &[crate::ast::PathSegment],
    bound: &BTreeSet<String>,
    package: &Package<UncheckedPrime>,
    module: &Module<UncheckedPrime>,
    module_path: &str,
    out: &mut BTreeSet<FnIdentity>,
) {
    if let Some(identity) = resolve_fn_identity(segments, bound, package, module, module_path) {
        out.insert(identity);
    }
}

fn resolve_fn_identity(
    segments: &[crate::ast::PathSegment],
    bound: &BTreeSet<String>,
    package: &Package<UncheckedPrime>,
    module: &Module<UncheckedPrime>,
    module_path: &str,
) -> Option<FnIdentity> {
    match segments {
        [name] if !bound.contains(&name.name) => {
            if find_module_fn(module, &name.name).is_some() {
                return Some(canonical_fn_identity(module_path, &name.name));
            }
            module.imports.iter().find_map(|u| {
                let ImportKind::Selective { items, from } = &u.kind else {
                    return None;
                };
                items
                    .iter()
                    .filter_map(crate::ast::ImportItem::as_name)
                    .any(|imported| imported == name.name)
                    .then(|| module_import_path(from))
                    .and_then(|target_path| {
                        package
                            .module(&target_path)
                            .map(|entry| (target_path, entry))
                    })
                    .and_then(|(target_path, entry)| {
                        find_module_fn(&entry.module, &name.name)
                            .map(|_| canonical_fn_identity(&target_path, &name.name))
                    })
            })
        }
        [alias, name] => module.imports.iter().find_map(|u| {
            let ImportKind::Qualified {
                path,
                alias: import_alias,
            } = &u.kind
            else {
                return None;
            };
            if import_alias != &alias.name {
                return None;
            }
            let target_path = module_import_path(path);
            let entry = package.module(&target_path)?;
            let def = find_module_fn(&entry.module, &name.name)?;
            crate::pass::resolve::is_visible(&def.vis, &module.path)
                .then(|| canonical_fn_identity(&target_path, &name.name))
        }),
        _ => None,
    }
}

fn canonical_fn_identity(module_path: &str, name: &str) -> FnIdentity {
    FnIdentity::Canonical {
        module_path: module_path.to_owned(),
        name: name.to_owned(),
    }
}

fn find_module_fn<'a>(
    module: &'a Module<UncheckedPrime>,
    name: &str,
) -> Option<&'a FnDef<UncheckedPrime>> {
    module.items.iter().find_map(|item| match item {
        Item::FnDef(def) if def.name == name => Some(def),
        _ => None,
    })
}

fn module_import_path(path: &ModulePath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn push_value_params(
    bound: &mut BTreeSet<String>,
    sig: &crate::ast::Signature<UncheckedPrime>,
) -> Vec<String> {
    let mut added = Vec::new();
    for p in &sig.params {
        if let SignatureParam::Value(vp) = p
            && bound.insert(vp.name.clone())
        {
            added.push(vp.name.clone());
        }
    }
    added
}

fn pop_names(bound: &mut BTreeSet<String>, names: Vec<String>) {
    for name in names {
        bound.remove(&name);
    }
}

fn write_framed(h: &mut Hasher, bytes: &[u8]) {
    let len = bytes.len() as u64;
    h.update(&len.to_le_bytes());
    h.update(bytes);
}

fn write_stable_ast_framed<T: serde::Serialize>(h: &mut Hasher, value: &T) {
    // Streaming uses declared struct-field order instead of the sorted
    // object order of an intermediate `serde_json::Value`. The compiler
    // cache ID hashes this source, so a serialization change invalidates
    // older entries before their keys can be reused. Input maps are
    // rejected: key-input projections must sort them into sequences first.
    let mut stable_hash = Hasher::new();
    write_framed(&mut stable_hash, b"equiv-cache-stable-ast-json-v1");
    serde_json::to_writer(HasherWriter(&mut stable_hash), &StableAst(value))
        .expect("equiv cache key input JSON-encodes while hashing");
    write_framed(h, stable_hash.finalize().as_bytes());
}

struct HasherWriter<'a>(&'a mut Hasher);

impl Write for HasherWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct StableAst<'a, T: ?Sized>(&'a T);

impl<T: serde::Serialize + ?Sized> serde::Serialize for StableAst<'_, T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(StableAstSerializer(serializer))
    }
}

#[derive(Clone, Copy)]
enum StableField {
    Ordinary,
    Span,
    Null,
}

fn stable_field(key: &str) -> StableField {
    if key == "span" || key.ends_with("_span") {
        StableField::Span
    } else if key == "ext" || key == "match_id" {
        StableField::Null
    } else {
        StableField::Ordinary
    }
}

struct StableSpan;

impl serde::Serialize for StableSpan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        // Stable AST keys erase source locations to this canonical zero-span
        // shape. Serialization bytes feed the hash directly, so fixed field
        // order is part of digest stability.
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("end", &0_u64)?;
        map.serialize_entry("start", &0_u64)?;
        map.end()
    }
}

struct StableAstSerializer<S>(S);

macro_rules! delegate_primitive {
    ($(fn $method:ident($ty:ty);)*) => {
        $(fn $method(self, value: $ty) -> Result<Self::Ok, Self::Error> {
            self.0.$method(value)
        })*
    };
}

impl<S: serde::Serializer> serde::Serializer for StableAstSerializer<S> {
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = StableSeq<S::SerializeSeq>;
    type SerializeTuple = StableTuple<S::SerializeTuple>;
    type SerializeTupleStruct = StableTupleStruct<S::SerializeTupleStruct>;
    type SerializeTupleVariant = StableTupleVariant<S::SerializeTupleVariant>;
    type SerializeMap = serde::ser::Impossible<S::Ok, S::Error>;
    type SerializeStruct = StableStruct<S::SerializeStruct>;
    type SerializeStructVariant = StableStructVariant<S::SerializeStructVariant>;

    delegate_primitive! {
        fn serialize_bool(bool);
        fn serialize_i8(i8);
        fn serialize_i16(i16);
        fn serialize_i32(i32);
        fn serialize_i64(i64);
        fn serialize_i128(i128);
        fn serialize_u8(u8);
        fn serialize_u16(u16);
        fn serialize_u32(u32);
        fn serialize_u64(u64);
        fn serialize_u128(u128);
        fn serialize_f32(f32);
        fn serialize_f64(f64);
        fn serialize_char(char);
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_str(value)
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_bytes(value)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_none()
    }

    fn serialize_some<T: serde::Serialize + ?Sized>(
        self,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_some(&StableAst(value))
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_unit()
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_unit_struct(name)
    }

    fn serialize_unit_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_unit_variant(name, variant_index, variant)
    }

    fn serialize_newtype_struct<T: serde::Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.0.serialize_newtype_struct(name, &StableAst(value))
    }

    fn serialize_newtype_variant<T: serde::Serialize + ?Sized>(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.0
            .serialize_newtype_variant(name, variant_index, variant, &StableAst(value))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        self.0.serialize_seq(len).map(StableSeq)
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.0.serialize_tuple(len).map(StableTuple)
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.0
            .serialize_tuple_struct(name, len)
            .map(StableTupleStruct)
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.0
            .serialize_tuple_variant(name, variant_index, variant, len)
            .map(StableTupleVariant)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(serde::ser::Error::custom(
            "equiv cache key maps must be projected into sorted sequences",
        ))
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.0.serialize_struct(name, len).map(StableStruct)
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.0
            .serialize_struct_variant(name, variant_index, variant, len)
            .map(StableStructVariant)
    }

    fn collect_str<T: std::fmt::Display + ?Sized>(
        self,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.0.collect_str(value)
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}

struct StableSeq<S>(S);

impl<S: serde::ser::SerializeSeq> serde::ser::SerializeSeq for StableSeq<S> {
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_element<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.0.serialize_element(&StableAst(value))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

struct StableTuple<S>(S);

impl<S: serde::ser::SerializeTuple> serde::ser::SerializeTuple for StableTuple<S> {
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_element<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.0.serialize_element(&StableAst(value))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

struct StableTupleStruct<S>(S);

impl<S: serde::ser::SerializeTupleStruct> serde::ser::SerializeTupleStruct
    for StableTupleStruct<S>
{
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_field<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.0.serialize_field(&StableAst(value))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

struct StableTupleVariant<S>(S);

impl<S: serde::ser::SerializeTupleVariant> serde::ser::SerializeTupleVariant
    for StableTupleVariant<S>
{
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_field<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.0.serialize_field(&StableAst(value))
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

struct StableStruct<S>(S);

impl<S: serde::ser::SerializeStruct> serde::ser::SerializeStruct for StableStruct<S> {
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_field<T: serde::Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        match stable_field(key) {
            StableField::Ordinary => self.0.serialize_field(key, &StableAst(value)),
            StableField::Span => self.0.serialize_field(key, &StableSpan),
            StableField::Null => self.0.serialize_field(key, &Option::<()>::None),
        }
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

struct StableStructVariant<S>(S);

impl<S: serde::ser::SerializeStructVariant> serde::ser::SerializeStructVariant
    for StableStructVariant<S>
{
    type Ok = S::Ok;
    type Error = S::Error;

    fn serialize_field<T: serde::Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        match stable_field(key) {
            StableField::Ordinary => self.0.serialize_field(key, &StableAst(value)),
            StableField::Span => self.0.serialize_field(key, &StableSpan),
            StableField::Null => self.0.serialize_field(key, &Option::<()>::None),
        }
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0.end()
    }
}

fn read_entry(root: &Path, key: &EquivCacheKey) -> Result<Option<EquivCacheEntry>, String> {
    let path = entry_path(root, key);
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("open {}: {e}", DisplayPath(&path))),
    };
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("read {}: {e}", DisplayPath(&path)))?;
    let (header_end, header_fields) =
        parse_header(&buf).map_err(|reason| format!("stale {}: {reason}", DisplayPath(&path)))?;
    let body = &buf[header_end..];
    validate_header(&header_fields, key, body)
        .map_err(|reason| format!("stale {}: {reason}", DisplayPath(&path)))?;
    let entry: EquivCacheEntry = postcard::from_bytes(body)
        .map_err(|e| format!("stale {}: postcard decode failed: {e}", DisplayPath(&path)))?;
    Ok(Some(entry))
}

fn write_entry(root: &Path, key: &EquivCacheKey, entry: &EquivCacheEntry) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("mkdir {}: {e}", DisplayPath(&root)))?;
    let final_path = entry_path(root, key);
    let tmp_path = tempfile_path(root, key);
    let body = postcard::to_allocvec(entry).map_err(|e| format!("postcard encode failed: {e}"))?;
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(encode_header(key, &body).as_bytes())
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
        f.write_all(&body)
            .map_err(|e| format!("write {}: {e}", DisplayPath(&tmp_path)))?;
    }
    crate::cache::publish_temp_file(&tmp_path, &final_path, || {
        read_entry(root, key).ok().flatten().is_some()
    })
}

fn entry_path(root: &Path, key: &EquivCacheKey) -> PathBuf {
    root.join(format!("{}.bin", key.hex))
}

fn tempfile_path(root: &Path, key: &EquivCacheKey) -> PathBuf {
    let pid = std::process::id();
    let counter = TEMPFILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    root.join(format!("{}.bin.tmp.{pid}-{counter}", key.hex))
}

fn encode_header(key: &EquivCacheKey, body: &[u8]) -> String {
    format!(
        "KIO-EQUIV-CACHE namespace={namespace} impl={impl_tag} \
         compiler={compiler} features={features} compiler_cache_id={compiler_cache_id} \
         key={key} body={body_digest}\n",
        namespace = CACHE_NAMESPACE,
        impl_tag = IMPLEMENTATION_TAG,
        compiler = COMPILER_VERSION,
        features = FEATURE_SET,
        compiler_cache_id = COMPILER_CACHE_ID,
        key = key.hex,
        body_digest = blake3::hash(body).to_hex(),
    )
}

fn parse_header(buf: &[u8]) -> Result<(usize, HashMap<String, String>), String> {
    let newline_idx = buf
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| "missing header newline".to_owned())?;
    let header_bytes = &buf[..newline_idx];
    let header_str =
        std::str::from_utf8(header_bytes).map_err(|_| "header is not valid UTF-8".to_owned())?;
    let rest = header_str
        .strip_prefix("KIO-EQUIV-CACHE ")
        .ok_or_else(|| format!("missing magic: header `{header_str}`"))?;
    let mut fields = HashMap::new();
    for tok in rest.split(' ') {
        if let Some((k, v)) = tok.split_once('=') {
            fields.insert(k.to_owned(), v.to_owned());
        }
    }
    Ok((newline_idx + 1, fields))
}

fn validate_header(
    fields: &HashMap<String, String>,
    expected: &EquivCacheKey,
    body: &[u8],
) -> Result<(), String> {
    require_field(fields, "namespace", CACHE_NAMESPACE)?;
    require_field(fields, "impl", IMPLEMENTATION_TAG)?;
    require_field(fields, "compiler_cache_id", COMPILER_CACHE_ID)?;
    require_field(fields, "key", expected.hex())?;
    require_field(fields, "body", blake3::hash(body).to_hex().as_str())?;
    Ok(())
}

fn require_field(
    fields: &HashMap<String, String>,
    name: &str,
    expected: &str,
) -> Result<(), String> {
    let actual = fields
        .get(name)
        .ok_or_else(|| format!("header missing `{name}=`"))?;
    if actual != expected {
        return Err(format!(
            "{name} mismatch: file `{actual}`, running `{expected}`"
        ));
    }
    Ok(())
}

fn write_gitignore_if_absent(cache_root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(cache_root)?;
    let path = cache_root.join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => f.write_all(b"*\n!.gitignore\n"),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

fn log_probe(kind: &str, key: &EquivCacheKey, module_path: &str, equiv_name: &str, reason: &str) {
    let on = std::env::var("KIO_DEBUG_EQUIV_CACHE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    if !on {
        return;
    }
    if reason.is_empty() {
        eprintln!("equiv-cache: {kind} {} {module_path}.{equiv_name}", key.hex);
    } else {
        eprintln!(
            "equiv-cache: {kind} {} {module_path}.{equiv_name} ({reason})",
            key.hex
        );
    }
}

pub fn resolve_from_cache_field(
    workspace_root: &Path,
    cache: &crate::ast::BuildBlockCache,
) -> EquivCache {
    if !crate::cache::policy::caches_enabled() {
        return EquivCache::disabled();
    }
    match cache {
        crate::ast::BuildBlockCache::Path { path, .. } => {
            let abs = absolutize(workspace_root, path);
            match EquivCache::open(abs) {
                Ok(c) => c,
                Err(_) => EquivCache::disabled(),
            }
        }
        crate::ast::BuildBlockCache::Disabled { .. } => EquivCache::disabled(),
    }
}

fn absolutize(workspace_root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_eval_module(path: &str) -> Module<UncheckedPrime> {
        let span = crate::span::Span::new(0, 0);
        Module {
            path: ModulePath {
                segments: path
                    .split('/')
                    .map(|segment| crate::ast::PathSegment::new(segment.to_owned(), span))
                    .collect(),
                span,
            },
            imports: Vec::new(),
            items: Vec::new(),
            meta: crate::ast::Meta::new(span),
            doc: None,
        }
    }

    fn empty_eval_package() -> Package<UncheckedPrime> {
        Package::from_parts(BTreeMap::new(), None)
    }

    fn eval_unit_expr() -> Expr<UncheckedPrime> {
        Expr::Unit {
            occurrence: Default::default(),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
        }
    }

    fn eval_fn(name: &str, body: Expr<UncheckedPrime>) -> FnDef<UncheckedPrime> {
        let span = crate::span::Span::new(0, 0);
        FnDef {
            vis: crate::ast::Visibility::Private,
            purity: crate::ast::Purity::Impure,
            name: name.to_owned(),
            sig: Signature::new(Vec::new()),
            ret: crate::ast::Type::Unit {
                meta: crate::ast::Meta::new(span),
            },
            ret_elided: (),
            body,
            meta: crate::ast::Meta::new(span),
            doc: None,
        }
    }

    fn eval_alias(body: crate::ast::Type<UncheckedPrime>) -> Item<UncheckedPrime> {
        let span = crate::span::Span::new(0, 0);
        Item::TypeAlias(crate::ast::TypeAlias {
            vis: crate::ast::Visibility::Public,
            name: "Shape".to_owned(),
            name_span: span,
            type_params: Vec::new(),
            body,
            meta: crate::ast::Meta::new(span),
            editable_span: None,
            doc: None,
        })
    }

    fn eval_package(
        path: &str,
        imports: Vec<crate::ast::Import>,
        items: Vec<Item<UncheckedPrime>>,
        package_name: Option<&str>,
    ) -> Package<UncheckedPrime> {
        let mut module = empty_eval_module(path);
        module.imports = imports;
        module.items = items;
        let scope = crate::pass::resolve::TopLevelScope::build(&module).unwrap();
        let entry = crate::pass::resolve::ModuleEntry {
            file_path: PathBuf::from(format!("{path}.kio")),
            module,
            scope,
        };
        let package_file = package_name.map(|name| crate::pass::resolve::PackageFileEntry {
            file_path: PathBuf::from(format!("{name}.pkg.kio")),
            package_name: name.to_owned(),
            package_file: crate::ast::PackageFile {
                name: name.to_owned(),
                build: None,
                bridge: None,
                meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
            },
        });
        Package::from_parts(BTreeMap::from([(path.to_owned(), entry)]), package_file)
    }

    #[derive(serde::Serialize)]
    struct StableFixture {
        semantic: u64,
        span: u64,
        source_span: (u64, u64),
        ext: Vec<u64>,
        match_id: Option<u64>,
        nested: StableFixtureNested,
        items: Vec<StableFixtureNested>,
        variants: Vec<StableFixtureVariant>,
    }

    #[derive(serde::Serialize)]
    struct StableFixtureNested {
        ordinary: String,
        inner_span: u64,
    }

    #[derive(serde::Serialize)]
    enum StableFixtureVariant {
        Unit,
        Newtype(StableFixtureNested),
        Tuple(u64, StableFixtureNested),
        Struct { ordinary: u64, span: u64 },
    }

    fn stable_fixture(
        semantic: u64,
        span: u64,
        ext: Vec<u64>,
        match_id: Option<u64>,
    ) -> StableFixture {
        StableFixture {
            semantic,
            span,
            source_span: (span + 1, span + 2),
            ext,
            match_id,
            nested: StableFixtureNested {
                ordinary: "kept".to_owned(),
                inner_span: span + 3,
            },
            items: vec![StableFixtureNested {
                ordinary: "also-kept".to_owned(),
                inner_span: span + 4,
            }],
            variants: vec![
                StableFixtureVariant::Unit,
                StableFixtureVariant::Newtype(StableFixtureNested {
                    ordinary: "newtype-kept".to_owned(),
                    inner_span: span + 5,
                }),
                StableFixtureVariant::Tuple(
                    semantic + 2,
                    StableFixtureNested {
                        ordinary: "tuple-kept".to_owned(),
                        inner_span: span + 6,
                    },
                ),
                StableFixtureVariant::Struct {
                    ordinary: semantic + 3,
                    span: span + 7,
                },
            ],
        }
    }

    fn legacy_stable_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
        let mut json = serde_json::to_value(value).unwrap();
        legacy_sanitize_ast_json(&mut json);
        json
    }

    fn legacy_sanitize_ast_json(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    if key == "span" || key.ends_with("_span") {
                        *child = serde_json::json!({ "start": 0, "end": 0 });
                    } else if key == "ext" || key == "match_id" {
                        *child = serde_json::Value::Null;
                    } else {
                        legacy_sanitize_ast_json(child);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    legacy_sanitize_ast_json(child);
                }
            }
            _ => {}
        }
    }

    fn streamed_stable_value<T: serde::Serialize>(value: &T) -> serde_json::Value {
        let mut bytes = Vec::new();
        serde_json::to_writer(&mut bytes, &StableAst(value)).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn stable_digest<T: serde::Serialize>(value: &T) -> blake3::Hash {
        let mut h = Hasher::new();
        write_stable_ast_framed(&mut h, value);
        h.finalize()
    }

    #[test]
    fn streamed_stable_ast_preserves_the_sanitized_logical_tree() {
        let fixture = stable_fixture(7, 100, vec![1, 2], Some(3));
        assert_eq!(
            streamed_stable_value(&fixture),
            legacy_stable_value(&fixture),
        );
    }

    #[test]
    fn stable_ast_digest_ignores_only_sanitized_fields() {
        let base = stable_fixture(7, 100, vec![1, 2], Some(3));
        let sanitized_fields_changed = stable_fixture(7, 900, vec![8, 9], Some(10));
        let semantic_changed = stable_fixture(8, 100, vec![1, 2], Some(3));

        assert_eq!(stable_digest(&base), stable_digest(&base));
        assert_eq!(
            stable_digest(&base),
            stable_digest(&sanitized_fields_changed),
            "span, *_span, ext, and match_id fields must not perturb the key",
        );
        assert_ne!(
            stable_digest(&base),
            stable_digest(&semantic_changed),
            "ordinary semantic fields must perturb the key",
        );
    }

    #[test]
    fn streamed_stable_ast_rejects_unprojected_map_inputs() {
        fn assert_rejected<T: serde::Serialize>(map: &T) {
            let err = serde_json::to_writer(Vec::new(), &StableAst(map)).unwrap_err();
            assert!(
                err.to_string()
                    .contains("maps must be projected into sorted sequences"),
                "unexpected error: {err}",
            );
        }

        assert_rejected(&BTreeMap::from([("key", 1_u64)]));
        assert_rejected(&HashMap::from([("key", 1_u64)]));
    }

    #[test]
    fn malformed_entry_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = EquivCache::open(tmp.path().join("cache")).unwrap();
        let key = EquivCacheKey {
            hex: "a".repeat(64),
        };
        let root = match &cache.inner {
            Backend::Active { root, .. } => root.clone(),
            Backend::Disabled => unreachable!(),
        };
        fs::write(entry_path(&root, &key), b"not a cache entry").unwrap();
        assert!(cache.lookup(&key, "m", "e").is_none());
    }

    #[test]
    fn key_digest_covers_substituted_evaluator_terms() {
        let span = crate::span::Span::new(0, 0);
        let primitives = crate::normalization::EvalPrimitiveEnv::default();
        let registry = crate::normalization::NewtypeRegistry::default();
        let package = empty_eval_package();
        let module = empty_eval_module("fixture/module");
        let prelude = EquivCacheKeyPrelude::new(
            &EquivRenderMode::new("Never"),
            &package,
            &primitives,
            &registry,
        );
        let module_path = DeclaredModulePath::new("fixture/module");
        let name = "same_artifact_identity";
        let sig = Signature::<UncheckedPrime>::new(Vec::new());
        let term = |name: &str| Expr::<UncheckedPrime>::Path {
            occurrence: Default::default(),
            segments: vec![crate::ast::PathSegment::new(name.to_owned(), span)],
            meta: crate::ast::Meta::new(span),
            ext: (),
        };
        let first = EquivCacheKey::new(
            &prelude,
            &module_path,
            name,
            &sig,
            &[term("first")],
            &package,
            &module,
        );
        let second = EquivCacheKey::new(
            &prelude,
            &module_path,
            name,
            &sig,
            &[term("second")],
            &package,
            &module,
        );
        let same_artifact = EquivCacheKey::new(
            &prelude,
            &module_path,
            name,
            &sig,
            &[term("first")],
            &package,
            &module,
        );

        assert_ne!(first, second);
        assert_eq!(
            first, same_artifact,
            "only the substituted evaluator artifact contributes to the key",
        );
    }

    #[test]
    fn entry_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = EquivCache::open(tmp.path().join("cache")).unwrap();
        let key = EquivCacheKey {
            hex: "b".repeat(64),
        };
        let entry = EquivCacheEntry {
            passed: false,
            output: "  fail equiv `e` in m\n".to_owned(),
        };
        cache.store(&key, "m", "e", &entry);
        assert_eq!(cache.lookup(&key, "m", "e"), Some(entry));
    }

    #[test]
    fn store_repairs_a_non_file_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = EquivCache::open(tmp.path().join("cache")).unwrap();
        let key = EquivCacheKey {
            hex: "a".repeat(64),
        };
        let entry = EquivCacheEntry {
            passed: true,
            output: "recomputed".to_owned(),
        };
        let root = match &cache.inner {
            Backend::Active { root, .. } => root,
            Backend::Disabled => unreachable!(),
        };
        fs::create_dir(entry_path(root, &key)).expect("create invalid destination directory");

        assert!(cache.lookup(&key, "m", "e").is_none());
        cache.store(&key, "m", "e", &entry);
        assert_eq!(cache.lookup(&key, "m", "e"), Some(entry));
    }

    #[test]
    fn exact_key_rejects_a_swapped_valid_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = EquivCache::open(tmp.path().join("cache")).unwrap();
        let first_key = EquivCacheKey {
            hex: "c".repeat(64),
        };
        let second_key = EquivCacheKey {
            hex: "d".repeat(64),
        };
        let first_entry = EquivCacheEntry {
            passed: true,
            output: "first".to_owned(),
        };
        let second_entry = EquivCacheEntry {
            passed: false,
            output: "second".to_owned(),
        };
        cache.store(&first_key, "m", "first", &first_entry);
        cache.store(&second_key, "m", "second", &second_entry);
        let root = match &cache.inner {
            Backend::Active { root, .. } => root,
            Backend::Disabled => unreachable!(),
        };

        let first_bytes = fs::read(entry_path(root, &first_key)).unwrap();
        fs::write(entry_path(root, &second_key), first_bytes).unwrap();

        assert!(cache.lookup(&second_key, "m", "second").is_none());
        cache.store(&second_key, "m", "second", &second_entry);
        assert_eq!(cache.lookup(&second_key, "m", "second"), Some(second_entry));
    }

    #[test]
    fn body_digest_rejects_a_different_valid_result() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = EquivCache::open(tmp.path().join("cache")).unwrap();
        let first_key = EquivCacheKey {
            hex: "e".repeat(64),
        };
        let second_key = EquivCacheKey {
            hex: "f".repeat(64),
        };
        let first_entry = EquivCacheEntry {
            passed: true,
            output: "first".to_owned(),
        };
        let second_entry = EquivCacheEntry {
            passed: false,
            output: "second".to_owned(),
        };
        cache.store(&first_key, "m", "first", &first_entry);
        cache.store(&second_key, "m", "second", &second_entry);
        let root = match &cache.inner {
            Backend::Active { root, .. } => root,
            Backend::Disabled => unreachable!(),
        };
        let first_bytes = fs::read(entry_path(root, &first_key)).unwrap();
        let second_bytes = fs::read(entry_path(root, &second_key)).unwrap();
        let first_header_end = first_bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let second_header_end = second_bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        let mut swapped = first_bytes[..first_header_end].to_vec();
        swapped.extend_from_slice(&second_bytes[second_header_end..]);
        fs::write(entry_path(root, &first_key), swapped).unwrap();

        assert!(cache.lookup(&first_key, "m", "first").is_none());
        cache.store(&first_key, "m", "first", &first_entry);
        assert_eq!(cache.lookup(&first_key, "m", "first"), Some(first_entry));
    }

    /// The shared cache-key inputs are hashed once into an
    /// [`EquivCacheKeyPrelude`] before the parallel `equiv` fan-out.
    /// Correctness hinges on folding in every non-package shared input while
    /// leaving substituted terms and referenced definitions to each
    /// per-`equiv` key. Pin that the primitive environment, render mode, and
    /// newtype registry each move the digest, and that equal inputs hash
    /// equally so warm-cache hits stay reproducible.
    #[test]
    fn prelude_digest_covers_non_package_shared_inputs() {
        use crate::normalization::NewtypeRegistry;

        let render = EquivRenderMode::new("Never");
        let package = empty_eval_package();
        let primitives = crate::normalization::EvalPrimitiveEnv::default();
        let registry = NewtypeRegistry::default();

        let base = EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry);
        assert_eq!(
            base.digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry).digest,
            "equal inputs must hash to an equal prelude digest",
        );

        let mut primitives_changed = crate::normalization::EvalPrimitiveEnv::default();
        primitives_changed.enable_comptime_module("fixture/module");
        assert_ne!(
            base.digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives_changed, &registry).digest,
            "a primitive-environment change must change the prelude digest",
        );

        let mut registry_forward = NewtypeRegistry::default();
        let mut registry_reverse = NewtypeRegistry::default();
        for (projector, constructor) in [("N.p1", "N.c1"), ("N.p2", "N.c2")] {
            registry_forward
                .proj_ctor
                .insert(projector.to_owned(), constructor.to_owned());
        }
        for (projector, constructor) in [("N.p2", "N.c2"), ("N.p1", "N.c1")] {
            registry_reverse
                .proj_ctor
                .insert(projector.to_owned(), constructor.to_owned());
        }
        for (constructor, canonical, arity) in [("N.c1", "N.c1", 1), ("N.c2", "N.c1", 2)] {
            registry_forward.ctors.insert(constructor.to_owned());
            registry_forward
                .ctor_canonical
                .insert(constructor.to_owned(), canonical.to_owned());
            registry_forward
                .ctor_abi_arity
                .insert(constructor.to_owned(), arity);
        }
        for (constructor, canonical, arity) in [("N.c2", "N.c1", 2), ("N.c1", "N.c1", 1)] {
            registry_reverse.ctors.insert(constructor.to_owned());
            registry_reverse
                .ctor_canonical
                .insert(constructor.to_owned(), canonical.to_owned());
            registry_reverse
                .ctor_abi_arity
                .insert(constructor.to_owned(), arity);
        }
        assert_eq!(
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry_forward).digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry_reverse).digest,
            "registry insertion order must not perturb the prelude digest",
        );

        let mut registry_changed = NewtypeRegistry::default();
        registry_changed.ctors.insert("N.c".to_owned());
        assert_ne!(
            base.digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry_changed).digest,
            "a constructor-set change must change the prelude digest",
        );

        let mut registry_changed = NewtypeRegistry::default();
        registry_changed
            .proj_ctor
            .insert("N.p".to_owned(), "N.c".to_owned());
        assert_ne!(
            base.digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry_changed).digest,
            "a projector-to-constructor change must change the prelude digest",
        );

        let mut registry_changed = NewtypeRegistry::default();
        registry_changed
            .ctor_canonical
            .insert("N.c".to_owned(), "N.canonical".to_owned());
        assert_ne!(
            base.digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry_changed).digest,
            "a canonical-constructor change must change the prelude digest",
        );

        let mut registry_changed = NewtypeRegistry::default();
        registry_changed.ctor_abi_arity.insert("N.c".to_owned(), 2);
        assert_ne!(
            base.digest,
            EquivCacheKeyPrelude::new(&render, &package, &primitives, &registry_changed).digest,
            "a constructor-arity change must change the prelude digest",
        );

        assert_ne!(
            base.digest,
            EquivCacheKeyPrelude::new(
                &EquivRenderMode::new("Always"),
                &package,
                &primitives,
                &registry,
            )
            .digest,
            "a render-mode change must change the prelude digest",
        );
    }

    #[test]
    fn prelude_digest_excludes_only_ordinary_function_bodies_from_package() {
        let span = crate::span::Span::new(0, 0);
        let render = EquivRenderMode::new("Never");
        let primitives = crate::normalization::EvalPrimitiveEnv::default();
        let registry = crate::normalization::NewtypeRegistry::default();
        let unit_type = || crate::ast::Type::Unit {
            meta: crate::ast::Meta::new(span),
        };
        let bottom_type = || crate::ast::Type::Bottom {
            meta: crate::ast::Meta::new(span),
        };
        let package = |path: &str,
                       imports: Vec<crate::ast::Import>,
                       fn_name: &str,
                       fn_body: Expr<UncheckedPrime>,
                       alias_body: crate::ast::Type<UncheckedPrime>,
                       package_name: Option<&str>| {
            eval_package(
                path,
                imports,
                vec![
                    Item::FnDef(eval_fn(fn_name, fn_body)),
                    eval_alias(alias_body),
                ],
                package_name,
            )
        };
        let digest = |package: &Package<UncheckedPrime>| {
            EquivCacheKeyPrelude::new(&render, package, &primitives, &registry).digest
        };

        let base = package(
            "m",
            Vec::new(),
            "f",
            eval_unit_expr(),
            unit_type(),
            Some("p"),
        );
        let changed_body = package(
            "m",
            Vec::new(),
            "f",
            Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(eval_unit_expr()),
                body: Box::new(eval_unit_expr()),
                meta: crate::ast::Meta::new(span),
            },
            unit_type(),
            Some("p"),
        );
        assert_eq!(
            digest(&base),
            digest(&changed_body),
            "an ordinary function-body edit belongs only to reachable per-equiv closures",
        );

        let changed_alias = package(
            "m",
            Vec::new(),
            "f",
            eval_unit_expr(),
            bottom_type(),
            Some("p"),
        );
        assert_ne!(digest(&base), digest(&changed_alias));

        let changed_header = package(
            "m",
            Vec::new(),
            "renamed",
            eval_unit_expr(),
            unit_type(),
            Some("p"),
        );
        assert_ne!(digest(&base), digest(&changed_header));

        let changed_use = package(
            "m",
            vec![crate::ast::Import {
                trailing_trivia: Vec::new(),
                kind: ImportKind::Intrinsics,
                span,
                leading_trivia: Vec::new(),
            }],
            "f",
            eval_unit_expr(),
            unit_type(),
            Some("p"),
        );
        assert_ne!(digest(&base), digest(&changed_use));

        let changed_path = package(
            "renamed",
            Vec::new(),
            "f",
            eval_unit_expr(),
            unit_type(),
            Some("p"),
        );
        assert_ne!(digest(&base), digest(&changed_path));

        let changed_package_file = package(
            "m",
            Vec::new(),
            "f",
            eval_unit_expr(),
            unit_type(),
            Some("renamed"),
        );
        assert_ne!(digest(&base), digest(&changed_package_file));
    }
}

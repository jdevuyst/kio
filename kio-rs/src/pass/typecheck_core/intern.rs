#[cfg(feature = "lsp")]
use std::collections::BTreeMap;
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::PathBuf;
#[cfg(feature = "lsp")]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock, RwLock};

use crate::ast::{Kind, Meta, Phase, Type};

use super::MemoCtx;
#[cfg(feature = "surface")]
use super::UserElaboratorArtifactMemo;

pub struct TypeInterner<P>
where
    P: Phase,
{
    table: RwLock<HashMap<TypeKey, Arc<InternedTypeNode<P>>>>,
}

impl<P> Default for TypeInterner<P>
where
    P: Phase,
{
    fn default() -> Self {
        Self {
            table: RwLock::new(HashMap::new()),
        }
    }
}

struct PackageTypecheckCache<P>
where
    P: Phase,
{
    type_interner: Arc<TypeInterner<P>>,
    memo: Arc<MemoCtx<P>>,
}

impl<P> PackageTypecheckCache<P>
where
    P: Phase,
{
    fn new(type_interner: Arc<TypeInterner<P>>) -> Self {
        Self {
            type_interner,
            memo: Arc::new(MemoCtx::default()),
        }
    }

    pub(crate) fn type_interner(&self) -> &Arc<TypeInterner<P>> {
        &self.type_interner
    }

    pub(crate) fn memo(&self) -> &Arc<MemoCtx<P>> {
        &self.memo
    }
}

#[cfg(feature = "lsp")]
struct PackageTypecheckRevisionEntry<V, P>
where
    P: Phase,
{
    version: V,
    cache: Arc<PackageTypecheckCache<P>>,
}

#[cfg(feature = "lsp")]
pub(crate) struct PackageTypecheckRevisionStore<K, V, P>
where
    P: Phase,
{
    entries: Arc<Mutex<BTreeMap<K, PackageTypecheckRevisionEntry<V, P>>>>,
}

#[cfg(feature = "lsp")]
impl<K, V, P> Clone for PackageTypecheckRevisionStore<K, V, P>
where
    P: Phase,
{
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }
}

#[cfg(feature = "lsp")]
impl<K, V, P> Default for PackageTypecheckRevisionStore<K, V, P>
where
    P: Phase,
{
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[cfg(feature = "lsp")]
impl<K, V, P> PackageTypecheckRevisionStore<K, V, P>
where
    K: Clone + Ord,
    V: Clone + Eq,
    P: Phase,
{
    fn cache_for(&self, key: &K, version: &V) -> Arc<PackageTypecheckCache<P>> {
        let mut entries = self
            .entries
            .lock()
            .expect("package typecheck revision store lock poisoned");
        if let Some(entry) = entries.get(key)
            && &entry.version == version
        {
            return entry.cache.clone();
        }
        let cache = Arc::new(PackageTypecheckCache::new(
            Arc::new(TypeInterner::default()),
        ));
        entries.insert(
            key.clone(),
            PackageTypecheckRevisionEntry {
                version: version.clone(),
                cache: cache.clone(),
            },
        );
        cache
    }

    #[cfg(test)]
    pub(crate) fn scope_for(
        &self,
        key: &K,
        version: &V,
        package: &crate::pass::resolve::Package<P>,
    ) -> Arc<PackageTypecheckScope<P>> {
        PackageTypecheckScope::for_package_cache(package, self.cache_for(key, version))
    }

    pub(crate) fn scope_for_normalized(
        &self,
        key: &K,
        version: &V,
        normalized: &crate::pass::alpha_normalize::AlphaNormalizedPackage<P>,
    ) -> Arc<PackageTypecheckScope<P>>
    where
        P: crate::pass::visit_mut::TypecheckVisitPhase,
    {
        PackageTypecheckScope::for_normalized_package_cache(
            normalized,
            self.cache_for(key, version),
        )
    }

    #[cfg(test)]
    pub(crate) fn only_memo(&self) -> Option<Arc<MemoCtx<P>>> {
        let entries = self
            .entries
            .lock()
            .expect("package typecheck revision store lock poisoned");
        (entries.len() == 1)
            .then(|| entries.values().next().expect("one revision entry"))
            .map(|entry| entry.cache.memo().clone())
    }
}

#[derive(Clone, Copy)]
enum TypecheckOwner {
    Package(usize),
    Module {
        module: usize,
        package: Option<usize>,
    },
}

pub struct PackageTypecheckScope<P>
where
    P: Phase,
{
    cache: Arc<PackageTypecheckCache<P>>,
    owner: TypecheckOwner,
    #[cfg(feature = "surface")]
    alpha_presentation: Option<Arc<crate::pass::binder_presentation::BinderPresentation>>,
    #[cfg(feature = "surface")]
    user_elaborator_artifacts: Arc<UserElaboratorArtifactMemo>,
    identity_alias_newtypes: OnceLock<Arc<crate::pass::resolve::IdentityAliasNewtypeIndex>>,
}

impl<P> PackageTypecheckScope<P>
where
    P: Phase,
{
    fn new(
        cache: Arc<PackageTypecheckCache<P>>,
        owner: TypecheckOwner,
        alpha_presentation: Option<Arc<crate::pass::binder_presentation::BinderPresentation>>,
    ) -> Arc<Self> {
        #[cfg(not(feature = "surface"))]
        let _ = alpha_presentation;
        Arc::new(Self {
            cache,
            owner,
            #[cfg(feature = "surface")]
            alpha_presentation,
            #[cfg(feature = "surface")]
            user_elaborator_artifacts: Arc::new(UserElaboratorArtifactMemo::default()),
            identity_alias_newtypes: OnceLock::new(),
        })
    }

    pub(crate) fn identity_alias_newtypes(
        &self,
        module: &crate::ast::Module<P>,
        package: Option<&crate::pass::resolve::Package<P>>,
    ) -> Arc<crate::pass::resolve::IdentityAliasNewtypeIndex>
    where
        P: crate::pass::resolve::ResolvePhase,
    {
        self.assert_module(module, package);
        self.identity_alias_newtypes
            .get_or_init(|| {
                Arc::new(crate::pass::resolve::IdentityAliasNewtypeIndex::build(
                    module, package,
                ))
            })
            .clone()
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn fresh_for_package(
        package: &crate::pass::resolve::Package<P>,
        type_interner: Arc<TypeInterner<P>>,
    ) -> Arc<Self> {
        Self::new(
            Arc::new(PackageTypecheckCache::new(type_interner)),
            TypecheckOwner::Package(address(package)),
            None,
        )
    }

    pub(crate) fn fresh_for_normalized_package(
        normalized: &crate::pass::alpha_normalize::AlphaNormalizedPackage<P>,
        type_interner: Arc<TypeInterner<P>>,
    ) -> Arc<Self>
    where
        P: crate::pass::visit_mut::TypecheckVisitPhase,
    {
        Self::new(
            Arc::new(PackageTypecheckCache::new(type_interner)),
            TypecheckOwner::Package(address(normalized.package())),
            Some(normalized.presentation_arc()),
        )
    }

    pub(crate) fn fresh_for_normalized_module(
        module: &crate::pass::alpha_normalize::AlphaNormalizedModule<P>,
        package: &crate::pass::alpha_normalize::AlphaNormalizedPackage<P>,
        type_interner: Arc<TypeInterner<P>>,
    ) -> Arc<Self>
    where
        P: crate::pass::visit_mut::TypecheckVisitPhase,
    {
        let mut presentation = package.presentation().clone();
        presentation.extend(module.presentation());
        Self::new(
            Arc::new(PackageTypecheckCache::new(type_interner)),
            TypecheckOwner::Module {
                module: address(module.module()),
                package: Some(address(package.package())),
            },
            Some(Arc::new(presentation)),
        )
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn fresh_for_normalized_standalone_module(
        module: &crate::pass::alpha_normalize::AlphaNormalizedModule<P>,
        type_interner: Arc<TypeInterner<P>>,
    ) -> Arc<Self>
    where
        P: crate::pass::visit_mut::TypecheckVisitPhase,
    {
        Self::new(
            Arc::new(PackageTypecheckCache::new(type_interner)),
            TypecheckOwner::Module {
                module: address(module.module()),
                package: None,
            },
            Some(Arc::new(module.presentation().clone())),
        )
    }

    pub(crate) fn fresh_for_module(
        module: &crate::ast::Module<P>,
        package: Option<&crate::pass::resolve::Package<P>>,
        type_interner: Arc<TypeInterner<P>>,
    ) -> Arc<Self> {
        let owner = match package {
            Some(package) if package_contains_module(package, module) => {
                TypecheckOwner::Package(address(package))
            }
            _ => TypecheckOwner::Module {
                module: address(module),
                package: package.map(address),
            },
        };
        Self::new(
            Arc::new(PackageTypecheckCache::new(type_interner)),
            owner,
            None,
        )
    }

    #[cfg(all(feature = "lsp", test))]
    fn for_package_cache(
        package: &crate::pass::resolve::Package<P>,
        cache: Arc<PackageTypecheckCache<P>>,
    ) -> Arc<Self> {
        Self::new(cache, TypecheckOwner::Package(address(package)), None)
    }

    #[cfg(feature = "lsp")]
    fn for_normalized_package_cache(
        normalized: &crate::pass::alpha_normalize::AlphaNormalizedPackage<P>,
        cache: Arc<PackageTypecheckCache<P>>,
    ) -> Arc<Self>
    where
        P: crate::pass::visit_mut::TypecheckVisitPhase,
    {
        Self::new(
            cache,
            TypecheckOwner::Package(address(normalized.package())),
            Some(normalized.presentation_arc()),
        )
    }

    pub(crate) fn assert_package(&self, package: &crate::pass::resolve::Package<P>) {
        assert!(
            matches!(
                self.owner,
                TypecheckOwner::Package(owner) if owner == address(package)
            ) || matches!(
                self.owner,
                TypecheckOwner::Module {
                    package: Some(owner),
                    ..
                } if owner == address(package)
            ),
            "package typecheck scope used with a different package analysis"
        );
    }

    #[cfg(feature = "surface")]
    pub(crate) fn try_renormalize_checked_for<E, F>(
        &self,
        package: &crate::pass::resolve::Package<P>,
        transform: F,
    ) -> Result<crate::pass::alpha_normalize::AlphaNormalizedPackage<P>, E>
    where
        P: crate::pass::visit_mut::TypecheckVisitPhase,
        F: FnOnce(&crate::pass::resolve::Package<P>) -> Result<crate::pass::resolve::Package<P>, E>,
    {
        self.assert_package(package);
        Ok(crate::pass::alpha_normalize::renormalize_checked_package(
            transform(package)?,
            self.alpha_presentation
                .as_ref()
                .expect("normalized package work requires a normalization-bearing scope")
                .clone(),
        ))
    }

    #[cfg(feature = "surface")]
    pub(crate) fn alpha_presentation(
        &self,
    ) -> &Arc<crate::pass::binder_presentation::BinderPresentation> {
        self.alpha_presentation
            .as_ref()
            .expect("normalized package work requires a normalization-bearing scope")
    }

    pub(crate) fn assert_module(
        &self,
        module: &crate::ast::Module<P>,
        package: Option<&crate::pass::resolve::Package<P>>,
    ) {
        match self.owner {
            TypecheckOwner::Package(owner) => {
                let package = package.expect("package typecheck scope requires its package");
                assert_eq!(
                    owner,
                    address(package),
                    "package typecheck scope used with a different package analysis"
                );
                assert!(
                    package_contains_module(package, module),
                    "package typecheck scope used with a module outside its package"
                );
            }
            TypecheckOwner::Module {
                module: owner_module,
                package: owner_package,
            } => match (owner_package, package) {
                (Some(owner_package), Some(package)) => {
                    assert_eq!(
                        owner_package,
                        address(package),
                        "module typecheck scope used with a different package context"
                    );
                    assert!(
                        owner_module == address(module) || package_contains_module(package, module),
                        "module typecheck scope used outside its root and bound package"
                    );
                }
                (None, None) => assert_eq!(
                    owner_module,
                    address(module),
                    "module typecheck scope used with a different module"
                ),
                _ => panic!("module typecheck scope used with a different package context"),
            },
        }
    }

    pub(crate) fn type_interner(&self) -> &Arc<TypeInterner<P>> {
        self.cache.type_interner()
    }

    pub(crate) fn memo(&self) -> &Arc<MemoCtx<P>> {
        self.cache.memo()
    }

    #[cfg(feature = "surface")]
    pub(crate) fn user_elaborator_artifacts(&self) -> &Arc<UserElaboratorArtifactMemo> {
        &self.user_elaborator_artifacts
    }

    #[cfg(feature = "cli")]
    pub(crate) fn user_elaborator_timing_snapshot(&self) -> super::UserElaboratorTimingSnapshot {
        let snapshot = self.memo().snapshot().user_elaborator_timings;
        #[cfg(feature = "surface")]
        {
            let mut snapshot = snapshot;
            let artifact = self.user_elaborator_artifacts.snapshot();
            snapshot.artifact_hits = artifact.hits;
            snapshot.artifact_misses = artifact.misses;
            snapshot.artifact_first_writes = artifact.first_writes;
            snapshot
        }
        #[cfg(not(feature = "surface"))]
        {
            snapshot
        }
    }
}

fn address<T>(value: &T) -> usize {
    std::ptr::from_ref(value).cast::<()>() as usize
}

fn package_contains_module<P: Phase>(
    package: &crate::pass::resolve::Package<P>,
    module: &crate::ast::Module<P>,
) -> bool {
    let module_path = module
        .path
        .segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    package
        .module(&module_path)
        .is_some_and(|entry| std::ptr::eq(&entry.module, module))
}

impl<P> TypeInterner<P>
where
    P: Phase + Clone,
{
    pub fn intern(&self, ty: &Type<P>) -> InternedType<P> {
        if super::type_contains_goal(ty) {
            // Open goals are call-local inference capabilities. Package-wide
            // interning would retain that transient authority beyond its
            // owning store even though the process-unique identity prevents
            // accidental equality with another store.
            return InternedType::fresh(ty.clone());
        }
        let key = TypeKey::from_type(ty);
        {
            let table = self.table.read().expect("type interner lock poisoned");
            if let Some(node) = table.get(&key) {
                return InternedType::from_shared_node(Arc::clone(node), ty);
            }
        }
        let mut table = self.table.write().expect("type interner lock poisoned");
        if let Some(node) = table.get(&key) {
            return InternedType::from_shared_node(Arc::clone(node), ty);
        }
        let hash = key_hash(&key);
        let node = Arc::new(InternedTypeNode {
            hash,
            ty: ty.clone(),
        });
        table.insert(key, Arc::clone(&node));
        InternedType {
            node,
            exact: None,
            identity_canonical: false,
            requirement_source: None,
        }
    }
}

/// A written requirement in the package snapshot being typechecked. This is
/// diagnostic presentation only; AST conversion and semantic interning omit it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RequirementSource {
    pub(crate) file: PathBuf,
    pub(crate) span: crate::span::Span,
}

#[derive(Debug)]
pub struct InternedType<P>
where
    P: Phase,
{
    node: Arc<InternedTypeNode<P>>,
    // The semantic node is shared across alpha-equivalent/source-distinct
    // occurrences. Retain an occurrence's exact representation only when it
    // differs from the node's concrete view.
    exact: Option<Arc<Type<P>>>,
    // Handle-local semantic provenance. `true` means every nominal head in
    // this type has already been resolved to its package identity. Keeping
    // this off the interned AST node keeps resolution metadata in the
    // typechecker's phase artifact while the Tree-That-Grows AST remains
    // purely structural.
    identity_canonical: bool,
    // Explanatory source belongs to this occurrence, never the shared semantic
    // node. It cannot grant typing or resolution authority to the type.
    requirement_source: Option<Arc<RequirementSource>>,
}

impl<P> Clone for InternedType<P>
where
    P: Phase,
{
    fn clone(&self) -> Self {
        Self {
            node: Arc::clone(&self.node),
            exact: self.exact.clone(),
            identity_canonical: self.identity_canonical,
            requirement_source: self.requirement_source.clone(),
        }
    }
}

impl<P> InternedType<P>
where
    P: Phase + Clone,
{
    pub fn fresh(ty: Type<P>) -> Self {
        let key = TypeKey::from_type(&ty);
        Self {
            node: Arc::new(InternedTypeNode {
                hash: key_hash(&key),
                ty,
            }),
            exact: None,
            identity_canonical: false,
            requirement_source: None,
        }
    }

    fn from_shared_node(node: Arc<InternedTypeNode<P>>, ty: &Type<P>) -> Self {
        let exact = (!representation_exact(&node.ty, ty)).then(|| Arc::new(ty.clone()));
        Self {
            node,
            exact,
            identity_canonical: false,
            requirement_source: None,
        }
    }

    pub(crate) fn representation_is_exact(&self, other: &Self) -> bool {
        self.requirement_source == other.requirement_source
            && representation_exact(self.as_type(), other.as_type())
    }

    pub(crate) fn requirement_source(&self) -> Option<&Arc<RequirementSource>> {
        self.requirement_source.as_ref()
    }

    pub(crate) fn with_requirement_source(
        mut self,
        source: Option<Arc<RequirementSource>>,
    ) -> Self {
        self.requirement_source = source;
        self
    }

    pub(crate) fn preserving_requirement_from(self, source: &Self) -> Self {
        self.with_requirement_source(source.requirement_source.clone())
    }

    /// Refine only a directly written subtree, before normalization or substitution.
    pub(crate) fn projecting_written_requirement_from(self, source: &Self) -> Self {
        let requirement = source.requirement_source.as_ref().map(|source| {
            Arc::new(RequirementSource {
                file: source.file.clone(),
                span: self.as_type().span(),
            })
        });
        self.with_requirement_source(requirement)
    }

    pub(crate) fn fresh_canonical(ty: Type<P>) -> Self {
        let mut handle = Self::fresh(ty);
        handle.identity_canonical = true;
        handle
    }

    pub(crate) fn fresh_with_identity(ty: Type<P>, identity_canonical: bool) -> Self {
        if identity_canonical {
            Self::fresh_canonical(ty)
        } else {
            Self::fresh(ty)
        }
    }

    pub(crate) fn identity_is_canonical(&self) -> bool {
        self.identity_canonical
    }

    pub(crate) fn with_canonical_identity(mut self) -> Self {
        self.identity_canonical = true;
        self
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }

    pub fn handle_hash(&self) -> u64 {
        self.node.hash
    }

    pub fn as_type(&self) -> &Type<P> {
        self.exact.as_deref().unwrap_or(&self.node.ty)
    }

    pub fn clone_type(&self) -> Type<P> {
        self.as_type().clone()
    }
}

impl<P> Deref for InternedType<P>
where
    P: Phase,
{
    type Target = Type<P>;

    fn deref(&self) -> &Self::Target {
        self.exact.as_deref().unwrap_or(&self.node.ty)
    }
}

fn meta_exact<P: Phase>(left: &Meta<P>, right: &Meta<P>) -> bool {
    left.span == right.span
        && left.leading_trivia == right.leading_trivia
        && left.trailing_trivia == right.trailing_trivia
}

/// Concrete-representation equality for an interned type occurrence.
///
/// `Type`'s ordinary equality deliberately ignores `PathSegment::span`, while
/// the semantic interner additionally ignores source metadata, forall names,
/// ABI arity, capabilities, and phase extensions. This comparator covers all
/// of those fields without changing semantic keys or requiring a stronger
/// phase bound.
fn representation_exact<P: Phase>(left: &Type<P>, right: &Type<P>) -> bool {
    match (left, right) {
        (
            Type::Path {
                segments: ls,
                args: la,
                meta: lm,
            },
            Type::Path {
                segments: rs,
                args: ra,
                meta: rm,
            },
        ) => {
            meta_exact(lm, rm)
                && ls.len() == rs.len()
                && ls
                    .iter()
                    .zip(rs)
                    .all(|(l, r)| l.name == r.name && l.span == r.span)
                && types_representation_exact(la, ra)
        }
        (Type::Unit { meta: l }, Type::Unit { meta: r })
        | (Type::Bottom { meta: l }, Type::Bottom { meta: r }) => meta_exact(l, r),
        (
            Type::Function {
                param: lp,
                ret: lr,
                meta: lm,
                abi_arity: la,
                caps: lc,
            },
            Type::Function {
                param: rp,
                ret: rr,
                meta: rm,
                abi_arity: ra,
                caps: rc,
            },
        ) => {
            meta_exact(lm, rm)
                && la == ra
                && lc == rc
                && representation_exact(lp, rp)
                && representation_exact(lr, rr)
        }
        (
            Type::Product {
                left: ll,
                right: lr,
                meta: lm,
            },
            Type::Product {
                left: rl,
                right: rr,
                meta: rm,
            },
        )
        | (
            Type::Sum {
                left: ll,
                right: lr,
                meta: lm,
            },
            Type::Sum {
                left: rl,
                right: rr,
                meta: rm,
            },
        ) => meta_exact(lm, rm) && representation_exact(ll, rl) && representation_exact(lr, rr),
        (
            Type::LabelSugar {
                labels: ll,
                meta: lm,
                ext: le,
            },
            Type::LabelSugar {
                labels: rl,
                meta: rm,
                ext: re,
            },
        ) => {
            meta_exact(lm, rm)
                && le == re
                && ll.len() == rl.len()
                && ll.iter().zip(rl).all(|(l, r)| {
                    l.label == r.label
                        && l.label_span == r.label_span
                        && meta_exact(&l.meta, &r.meta)
                        && option_type_representation_exact(&l.payload, &r.payload)
                })
        }
        (Type::Infer { meta: lm, ext: le }, Type::Infer { meta: rm, ext: re }) => {
            meta_exact(lm, rm) && le == re
        }
        (
            Type::Goal {
                goal: lg,
                args: la,
                meta: lm,
                ext: le,
            },
            Type::Goal {
                goal: rg,
                args: ra,
                meta: rm,
                ext: re,
            },
        ) => lg == rg && meta_exact(lm, rm) && le == re && types_representation_exact(la, ra),
        (
            Type::Forall {
                param: lp,
                body: lb,
                meta: lm,
            },
            Type::Forall {
                param: rp,
                body: rb,
                meta: rm,
            },
        ) => {
            lp.name == rp.name
                && lp.span == rp.span
                && lp.kind == rp.kind
                && meta_exact(lm, rm)
                && representation_exact(lb, rb)
        }
        _ => false,
    }
}

fn types_representation_exact<P: Phase>(left: &[Type<P>], right: &[Type<P>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(l, r)| representation_exact(l, r))
}

fn option_type_representation_exact<P: Phase>(
    left: &Option<Type<P>>,
    right: &Option<Type<P>>,
) -> bool {
    match (left, right) {
        (Some(l), Some(r)) => representation_exact(l, r),
        (None, None) => true,
        _ => false,
    }
}

impl<P> PartialEq for InternedType<P>
where
    P: Phase,
{
    fn eq(&self, other: &Self) -> bool {
        self.identity_canonical == other.identity_canonical && Arc::ptr_eq(&self.node, &other.node)
    }
}

impl<P> Eq for InternedType<P> where P: Phase {}

impl<P> Hash for InternedType<P>
where
    P: Phase,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.node.hash);
        state.write_u8(u8::from(self.identity_canonical));
    }
}

#[cfg(feature = "surface")]
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub(crate) struct TypeMemoKey(TypeKey);

#[cfg(feature = "surface")]
impl TypeMemoKey {
    pub(crate) fn from_type<P>(ty: &Type<P>) -> Self
    where
        P: Phase,
    {
        Self::try_from_type(ty).expect(
            "an open type-inference goal cannot enter a persistent memo key; close and zonk its owning domain first",
        )
    }

    pub(crate) fn try_from_type<P>(ty: &Type<P>) -> Option<Self>
    where
        P: Phase,
    {
        (!super::type_contains_goal(ty)).then(|| Self(TypeKey::from_type(ty)))
    }
}

/// Structural key for one transient type-inference snapshot. Unlike
/// `TypeMemoKey`, this admits call-local goal identities and must never cross
/// the owning typecheck frontier.
#[cfg(all(feature = "surface", test))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TransientTypeKey(TypeKey);

#[cfg(all(feature = "surface", test))]
impl TransientTypeKey {
    pub(crate) fn from_type<P>(ty: &Type<P>) -> Self
    where
        P: Phase,
    {
        Self(TypeKey::from_type(ty))
    }
}

#[derive(Debug)]
struct InternedTypeNode<P>
where
    P: Phase,
{
    hash: u64,
    ty: Type<P>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
enum TypeKey {
    Path {
        head: TypeHead,
        args: Vec<TypeKey>,
    },
    Unit,
    Bottom,
    Function {
        param: Box<TypeKey>,
        ret: Box<TypeKey>,
    },
    Product {
        left: Box<TypeKey>,
        right: Box<TypeKey>,
    },
    Sum {
        left: Box<TypeKey>,
        right: Box<TypeKey>,
    },
    Forall {
        param: Option<KindKey>,
        body: Box<TypeKey>,
    },
    LabelSugar(Vec<(String, Option<TypeKey>)>),
    Infer,
    Goal {
        goal: crate::ast::TypeGoalRef,
        args: Vec<TypeKey>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
enum TypeHead {
    Named(Vec<String>),
    Bound(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
enum KindKey {
    Star,
    Arrow(Box<KindKey>, Box<KindKey>),
}

impl KindKey {
    fn from_kind(kind: &Kind) -> Self {
        match kind {
            Kind::Star => KindKey::Star,
            Kind::Arrow(param, ret) => KindKey::Arrow(
                Box::new(Self::from_kind(param)),
                Box::new(Self::from_kind(ret)),
            ),
        }
    }
}

impl TypeKey {
    fn from_type<P>(ty: &Type<P>) -> Self
    where
        P: Phase,
    {
        let mut binders = Vec::new();
        Self::from_type_with_binders(ty, &mut binders)
    }

    fn from_type_with_binders<P>(ty: &Type<P>, binders: &mut Vec<String>) -> Self
    where
        P: Phase,
    {
        match ty {
            Type::Path { segments, args, .. } => {
                let head = if segments.len() == 1 {
                    let name = segments[0].name.as_str();
                    binders
                        .iter()
                        .rev()
                        .position(|binder| binder == name)
                        .map(TypeHead::Bound)
                        .unwrap_or_else(|| {
                            TypeHead::Named(segments.iter().map(|s| s.name.clone()).collect())
                        })
                } else {
                    TypeHead::Named(segments.iter().map(|s| s.name.clone()).collect())
                };
                TypeKey::Path {
                    head,
                    args: args
                        .iter()
                        .map(|arg| Self::from_type_with_binders(arg, binders))
                        .collect(),
                }
            }
            Type::Unit { .. } => TypeKey::Unit,
            Type::Bottom { .. } => TypeKey::Bottom,
            Type::Function { param, ret, .. } => TypeKey::Function {
                param: Box::new(Self::from_type_with_binders(param, binders)),
                ret: Box::new(Self::from_type_with_binders(ret, binders)),
            },
            Type::Product { left, right, .. } => TypeKey::Product {
                left: Box::new(Self::from_type_with_binders(left, binders)),
                right: Box::new(Self::from_type_with_binders(right, binders)),
            },
            Type::Sum { left, right, .. } => TypeKey::Sum {
                left: Box::new(Self::from_type_with_binders(left, binders)),
                right: Box::new(Self::from_type_with_binders(right, binders)),
            },
            Type::Forall { param, body, .. } => {
                let old_len = binders.len();
                binders.push(param.name.clone());
                let body = Box::new(Self::from_type_with_binders(body, binders));
                binders.truncate(old_len);
                TypeKey::Forall {
                    param: param.kind.as_ref().map(KindKey::from_kind),
                    body,
                }
            }
            Type::Infer { .. } => TypeKey::Infer,
            Type::Goal { goal, args, .. } => TypeKey::Goal {
                goal: *goal,
                args: args
                    .iter()
                    .map(|arg| Self::from_type_with_binders(arg, binders))
                    .collect(),
            },
            Type::LabelSugar { labels, .. } => {
                let mut labels: Vec<_> = labels
                    .iter()
                    .map(|label| {
                        (
                            label.label.clone(),
                            label
                                .payload
                                .as_ref()
                                .map(|payload| Self::from_type_with_binders(payload, binders)),
                        )
                    })
                    .collect();
                labels.sort_by(|a, b| a.0.cmp(&b.0));
                TypeKey::LabelSugar(labels)
            }
        }
    }
}

fn key_hash(key: &TypeKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use super::*;
    use crate::ast::{Lowered, Meta, PathSegment, TypeParam};
    use crate::span::Span;

    type TestType = Type<Lowered>;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn path(name: &str) -> TestType {
        Type::synth_path(vec![name.to_owned()], Vec::new(), sp())
    }

    fn path_at(name: &str, span: Span) -> TestType {
        Type::synth_path(vec![name.to_owned()], Vec::new(), span)
    }

    #[test]
    fn requirement_sources_are_occurrence_local_and_shared_by_clones() {
        let interner = TypeInterner::default();
        let ty = path_at("W", Span::new(10, 11));
        let first =
            interner
                .intern(&ty)
                .with_requirement_source(Some(Arc::new(RequirementSource {
                    file: PathBuf::from("provider.kio"),
                    span: ty.span(),
                })));
        let second =
            interner
                .intern(&ty)
                .with_requirement_source(Some(Arc::new(RequirementSource {
                    file: PathBuf::from("other.kio"),
                    span: ty.span(),
                })));
        assert!(first.ptr_eq(&second));
        assert_eq!(first.handle_hash(), second.handle_hash());
        assert!(!first.representation_is_exact(&second));
        let cloned = first.clone();
        assert!(Arc::ptr_eq(
            first.requirement_source().unwrap(),
            cloned.requirement_source().unwrap()
        ));
        assert!(interner.intern(&ty).requirement_source().is_none());
        let preserved = interner.intern(&ty).preserving_requirement_from(&first);
        assert!(Arc::ptr_eq(
            first.requirement_source().unwrap(),
            preserved.requirement_source().unwrap()
        ));
        let projected = interner
            .intern(&path_at("W", Span::new(20, 21)))
            .projecting_written_requirement_from(&first);
        assert!(!Arc::ptr_eq(
            first.requirement_source().unwrap(),
            projected.requirement_source().unwrap()
        ));
        assert_eq!(
            projected.requirement_source().unwrap().file,
            PathBuf::from("provider.kio")
        );
        assert_eq!(
            projected.requirement_source().unwrap().span,
            Span::new(20, 21)
        );
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(std::mem::size_of::<InternedType<Lowered>>(), 32);
            assert_eq!(
                std::mem::size_of::<RequirementSource>(),
                std::mem::size_of::<(PathBuf, Span)>()
            );
        }
    }

    fn forall(name: &str, body: TestType) -> TestType {
        forall_at(name, sp(), body)
    }

    fn forall_at(name: &str, span: Span, body: TestType) -> TestType {
        Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: Meta::new(span),
        }
    }

    fn polymorphic_identity(name: &str, span: Span) -> TestType {
        forall_at(
            name,
            span,
            Type::Function {
                param: Box::new(path_at(name, span)),
                ret: Box::new(path_at(name, span)),
                meta: Meta::new(span),
                abi_arity: 1,
                caps: (),
            },
        )
    }

    fn hash_handle(handle: &InternedType<Lowered>) -> u64 {
        let mut hasher = DefaultHasher::new();
        handle.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn interns_same_type_to_same_pointer() {
        let interner = TypeInterner::default();
        let a = Type::synth_function(vec![path("A")], path("B"), sp());
        let first = interner.intern(&a);
        let second = interner.intern(&a);
        assert!(first.ptr_eq(&second));
        assert_eq!(first, second);
        assert_eq!(first.handle_hash(), second.handle_hash());
        assert_eq!(hash_handle(&first), hash_handle(&second));
    }

    #[test]
    fn alpha_equivalent_handles_share_semantics_and_retain_both_insertion_orders() {
        for (first, second) in [
            (
                polymorphic_identity("A", Span::new(1, 8)),
                polymorphic_identity("B", Span::new(21, 28)),
            ),
            (
                polymorphic_identity("B", Span::new(21, 28)),
                polymorphic_identity("A", Span::new(1, 8)),
            ),
        ] {
            let interner = TypeInterner::default();
            let first_handle = interner.intern(&first);
            let second_handle = interner.intern(&second);
            assert!(first_handle.ptr_eq(&second_handle));
            assert_eq!(first_handle.handle_hash(), second_handle.handle_hash());
            assert!(representation_exact(first_handle.as_type(), &first));
            assert!(representation_exact(second_handle.as_type(), &second));
        }
    }

    #[test]
    fn exact_view_compares_path_segment_spans_even_when_root_meta_matches() {
        let root = Span::new(0, 20);
        let first: TestType = Type::Path {
            segments: vec![PathSegment::new("pkg".to_owned(), Span::new(0, 3))],
            args: Vec::new(),
            meta: Meta::new(root),
        };
        let second: TestType = Type::Path {
            segments: vec![PathSegment::new("pkg".to_owned(), Span::new(10, 13))],
            args: Vec::new(),
            meta: Meta::new(root),
        };
        assert_eq!(
            first, second,
            "ordinary Type equality ignores segment spans"
        );

        let interner = TypeInterner::default();
        let first_handle = interner.intern(&first);
        let second_handle = interner.intern(&second);
        assert!(first_handle.ptr_eq(&second_handle));
        assert!(representation_exact(second_handle.as_type(), &second));
        assert!(!representation_exact(second_handle.as_type(), &first));
    }

    #[test]
    fn semantically_equal_function_handles_retain_their_abi_arities() {
        let function = |abi_arity| Type::Function {
            param: Box::new(path("A")),
            ret: Box::new(path("B")),
            meta: Meta::new(sp()),
            abi_arity,
            caps: (),
        };
        let unary = function(1);
        let binary = function(2);
        let interner = TypeInterner::default();
        let unary_handle = interner.intern(&unary);
        let binary_handle = interner.intern(&binary);
        assert!(unary_handle.ptr_eq(&binary_handle));
        assert!(matches!(
            unary_handle.as_type(),
            Type::Function { abi_arity: 1, .. }
        ));
        assert!(matches!(
            binary_handle.as_type(),
            Type::Function { abi_arity: 2, .. }
        ));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn package_versions_share_structural_types_but_not_content_memos_or_analysis_artifacts() {
        let interner = std::sync::Arc::new(TypeInterner::default());
        let first_cache = std::sync::Arc::new(PackageTypecheckCache::new(interner.clone()));
        let second_cache = std::sync::Arc::new(PackageTypecheckCache::new(interner));
        let first =
            PackageTypecheckScope::new(first_cache.clone(), TypecheckOwner::Package(1), None);
        let repeated_first =
            PackageTypecheckScope::new(first_cache, TypecheckOwner::Package(1), None);
        let second = PackageTypecheckScope::new(second_cache, TypecheckOwner::Package(2), None);

        assert!(std::sync::Arc::ptr_eq(
            first.type_interner(),
            second.type_interner()
        ));
        assert!(!std::sync::Arc::ptr_eq(first.memo(), second.memo()));
        assert!(std::sync::Arc::ptr_eq(first.memo(), repeated_first.memo()));
        assert!(!std::sync::Arc::ptr_eq(
            first.user_elaborator_artifacts(),
            repeated_first.user_elaborator_artifacts(),
        ));
        assert!(!std::sync::Arc::ptr_eq(
            first.user_elaborator_artifacts(),
            second.user_elaborator_artifacts(),
        ));

        let first_type = first.type_interner().intern(&path("Shared"));
        let second_type = second.type_interner().intern(&path("Shared"));
        assert!(first_type.ptr_eq(&second_type));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn module_scope_admits_only_its_root_and_bound_package_members() {
        fn empty_module(path: &str) -> crate::ast::Module<crate::ast::Lowered> {
            let span = crate::span::Span::new(0, 0);
            crate::ast::Module {
                path: crate::ast::ModulePath {
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

        fn package_with(
            module: crate::ast::Module<crate::ast::Lowered>,
        ) -> crate::pass::resolve::Package<crate::ast::Lowered> {
            let file_name = format!(
                "{}.kio",
                module.path.segments.last().expect("module segment").name
            );
            crate::pass::resolve::Package::build(
                std::path::Path::new(""),
                vec![(std::path::PathBuf::from(file_name), module)],
                None,
            )
            .expect("package builds")
        }

        let root = empty_module("adapter");
        let other_root = empty_module("other_adapter");
        let package = package_with(empty_module("provider"));
        let other_package = package_with(empty_module("provider"));
        let provider = &package.module("provider").expect("provider").module;
        let cloned_provider = provider.clone();
        let other_provider = &other_package
            .module("provider")
            .expect("other provider")
            .module;
        let scope = PackageTypecheckScope::fresh_for_module(
            &root,
            Some(&package),
            std::sync::Arc::new(TypeInterner::default()),
        );

        scope.assert_module(&root, Some(&package));
        scope.assert_module(provider, Some(&package));
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scope.assert_module(&other_root, Some(&package));
            }))
            .is_err()
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scope.assert_module(&cloned_provider, Some(&package));
            }))
            .is_err()
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scope.assert_module(other_provider, Some(&other_package));
            }))
            .is_err()
        );

        let package_scope = PackageTypecheckScope::fresh_for_package(
            &package,
            std::sync::Arc::new(TypeInterner::default()),
        );
        package_scope.assert_module(provider, Some(&package));
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                package_scope.assert_module(&root, Some(&package));
            }))
            .is_err()
        );

        let standalone_scope = PackageTypecheckScope::fresh_for_module(
            &root,
            None,
            std::sync::Arc::new(TypeInterner::default()),
        );
        standalone_scope.assert_module(&root, None);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                standalone_scope.assert_module(&other_root, None);
            }))
            .is_err()
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                standalone_scope.assert_module(&root, Some(&package));
            }))
            .is_err()
        );
    }

    #[cfg(all(feature = "surface", feature = "lsp"))]
    #[test]
    fn revision_store_reuses_only_exact_versions_and_binds_each_scope() {
        let first_package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::new(),
            None,
        );
        let changed_package = crate::pass::resolve::Package::<Lowered>::from_parts(
            std::collections::BTreeMap::new(),
            None,
        );
        let revisions = PackageTypecheckRevisionStore::<String, String, Lowered>::default();
        let key = "package".to_owned();
        let first_version = "first".to_owned();
        let changed_version = "changed".to_owned();

        let first = revisions.scope_for(&key, &first_version, &first_package);
        let repeated = revisions.scope_for(&key, &first_version, &first_package);
        assert!(Arc::ptr_eq(first.memo(), repeated.memo()));
        assert!(!Arc::ptr_eq(
            first.user_elaborator_artifacts(),
            repeated.user_elaborator_artifacts()
        ));

        let changed = revisions.scope_for(&key, &changed_version, &changed_package);
        assert!(!Arc::ptr_eq(first.memo(), changed.memo()));
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                first.assert_package(&changed_package)
            }))
            .is_err(),
            "an analysis scope must reject a different package allocation"
        );
    }

    #[test]
    fn resolved_identity_provenance_separates_handle_equality_and_hashing() {
        let scoped = InternedType::fresh(Type::synth_path(
            vec!["owner".to_owned(), "Type".to_owned()],
            Vec::new(),
            sp(),
        ));
        let canonical = scoped.clone().with_canonical_identity();

        assert!(scoped.ptr_eq(&canonical));
        assert_ne!(scoped, canonical);
        assert_ne!(hash_handle(&scoped), hash_handle(&canonical));
    }

    #[test]
    fn canonical_key_ignores_source_spans() {
        let interner = TypeInterner::default();
        let first = interner.intern(&path_at("A", Span::new(0, 1)));
        let second = interner.intern(&path_at("A", Span::new(10, 11)));
        assert!(first.ptr_eq(&second));
    }

    #[test]
    fn open_goal_bypasses_package_global_interner() {
        let goal = Type::Goal {
            goal: crate::ast::TypeGoalRef::for_test(2, 3, 5),
            args: vec![path("A")],
            meta: Meta::new(sp()),
            ext: (),
        };
        let interner = TypeInterner::default();
        let first = interner.intern(&goal);
        let second = interner.intern(&goal);
        assert!(
            !first.ptr_eq(&second),
            "open goal handles must remain local rather than sharing a global node"
        );
        assert!(
            interner
                .table
                .read()
                .expect("type interner lock")
                .is_empty(),
            "open goals must not populate the package-global table"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn open_goal_refuses_package_global_memo_key() {
        let goal = Type::Goal {
            goal: crate::ast::TypeGoalRef::for_test(2, 3, 5),
            args: vec![path("A")],
            meta: Meta::new(sp()),
            ext: (),
        };
        assert!(TypeMemoKey::try_from_type(&goal).is_none());
        assert!(
            std::panic::catch_unwind(|| TypeMemoKey::from_type(&goal)).is_err(),
            "an open goal must not acquire package-global memo identity"
        );
    }

    #[test]
    fn canonicalizes_forall_binder_names() {
        let interner = TypeInterner::default();
        let lhs = forall("a", path("a"));
        let rhs = forall("b", path("b"));
        let first = interner.intern(&lhs);
        let second = interner.intern(&rhs);
        assert!(first.ptr_eq(&second));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn memo_key_matches_equivalent_fresh_handles() {
        let lhs = InternedType::fresh(forall("a", path("a")));
        let rhs = InternedType::fresh(forall("b", path("b")));
        assert!(!lhs.ptr_eq(&rhs));
        assert_eq!(
            TypeMemoKey::from_type(lhs.as_type()),
            TypeMemoKey::from_type(rhs.as_type())
        );
    }

    #[test]
    fn keeps_distinct_named_paths_apart() {
        let interner = TypeInterner::default();
        let lhs = interner.intern(&path("A"));
        let rhs = interner.intern(&path("B"));
        assert!(!lhs.ptr_eq(&rhs));
    }

    fn product(l: TestType, r: TestType) -> TestType {
        Type::Product {
            left: Box::new(l),
            right: Box::new(r),
            meta: Meta::new(sp()),
        }
    }

    fn sum(l: TestType, r: TestType) -> TestType {
        Type::Sum {
            left: Box::new(l),
            right: Box::new(r),
            meta: Meta::new(sp()),
        }
    }

    #[test]
    fn keeps_product_order_distinct() {
        let interner = TypeInterner::default();
        let lhs = interner.intern(&product(path("A"), path("B")));
        let rhs = interner.intern(&product(path("B"), path("A")));
        assert!(!lhs.ptr_eq(&rhs));
    }

    #[test]
    fn keeps_product_association_distinct() {
        let interner = TypeInterner::default();
        let lhs = product(path("A"), product(path("B"), path("C")));
        let rhs = product(product(path("A"), path("B")), path("C"));
        let first = interner.intern(&lhs);
        let second = interner.intern(&rhs);
        assert!(!first.ptr_eq(&second));
    }

    #[test]
    fn keeps_sum_order_distinct() {
        let interner = TypeInterner::default();
        let lhs = interner.intern(&sum(path("A"), path("B")));
        let rhs = interner.intern(&sum(path("B"), path("A")));
        assert!(!lhs.ptr_eq(&rhs));
    }

    #[test]
    fn keeps_sum_association_distinct() {
        let interner = TypeInterner::default();
        let lhs = sum(path("A"), sum(path("B"), path("C")));
        let rhs = sum(sum(path("A"), path("B")), path("C"));
        let first = interner.intern(&lhs);
        let second = interner.intern(&rhs);
        assert!(!first.ptr_eq(&second));
    }
}

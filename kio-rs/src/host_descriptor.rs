//! Backend-agnostic descriptor of a package's host-trait shape.
//!
//! A package's bridged modules declare `host type` / `host fn`
//! items, and the package file's `bridge` block selects which modules
//! reach the host. Together they commit a package to a host-side
//! contract: an interface the host must supply to instantiate the
//! package. Each backend renders that contract in its own
//! surface syntax, but the *shape* of the contract is determined by the
//! bridged host items alone, not by the backend.
//!
//! [`HostDescriptor`] is that backend-agnostic shape: a record of the
//! host items the package declares, plus the bound *intents* a typed
//! backend needs the host types to satisfy. Building it walks the
//! `Routed` package file once; per-backend renderers consume the
//! descriptor and translate each item to their surface syntax.
//!
//! ## Scope
//!
//! - The descriptor's `'a` lifetime ties it to the source `PackageFile<Routed>`.
//!   Renderers that need rich type information read the borrowed
//!   `Type<Routed>` references directly; the Rust backend joins those entries
//!   to its prepared facade catalog for exact boundary rendering.
//!   Owned-summary callers (the test runner's stub-synthesis path) project the
//!   descriptor through [`HostManifest`], a small serializable shape.
//! - The descriptor carries the host items *as the package file declared
//!   them*. The source-compatible `{ owned }` annotation selects no alternate
//!   facade: Rust renders `role(str)` values in owned form uniformly. Other
//!   backend-specific rendering decisions stay in the renderer, not in the
//!   descriptor.
//! - [`BoundRequirement`] names the *intent* behind each bound a typed
//!   backend needs the host type to satisfy. The Rust renderer translates
//!   intents to `Clone + 'static + PartialEq + …`. The descriptor commits
//!   to the intents; the surface syntax is per-backend.

use crate::ast::{HostFn, HostType, Item, Role, Routed, Type};
use crate::pass::resolve::Package;

/// Backend-agnostic descriptor of one package's host-trait shape.
///
/// Built from a `&PackageFile<Routed>` via [`build_host_descriptor`].
/// Renderers consume this to produce per-backend trait / interface
/// source.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDescriptor<'a> {
    /// One entry per `host type` declaration at this scope, in
    /// declaration order.
    pub host_types: Vec<HostTypeDecl<'a>>,
    /// One entry per `host fn` declaration at this scope, in
    /// declaration order.
    pub host_fns: Vec<HostFnDecl<'a>>,
    /// The package's last sealed contract generation `N-1` from its
    /// `*.sig.kio` `signature <pkg> v(N);` header, when the package ships a
    /// signature changelog. Author/CI/changelog bookkeeping only — the
    /// loader's structural contract match never consults it. `None` when
    /// no signature file is present.
    pub contract_version: Option<u32>,
    /// Host items that an earlier contract generation declared and a
    /// later one removed, recovered from the signature changelog's
    /// history. A backend whose host boundary is not removal-tolerant
    /// (Rust: removing a `host fn` orphans an existing `impl` method)
    /// re-emits these as deprecated boundary members so unchanged host
    /// source keeps compiling; removal-tolerant backends (JS) ignore
    /// them. Backend-independent: each carries the frozen signature and
    /// the version it was removed at; the per-backend deprecation policy
    /// (the window, the rendering) lives in each emitter. Empty when the
    /// package ships no signature file or removed no host item.
    pub deprecated_host_items: Vec<DeprecatedHostItem>,
}

/// One host item removed across the signature changelog's history,
/// recovered (frozen signature + removed-at version) for a backend's
/// deprecated re-emit. Owned — it comes from the signature file, not the
/// borrowed package source.
///
/// Backend-independent: the descriptor commits to *what* was removed and
/// *when*; *how long* a backend re-emits it (the deprecation window) and
/// *how* it renders the deprecation are the emitter's policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecatedHostItem {
    /// The slash-path of the declaring module — the namespaced host
    /// boundary's key (same role as [`HostTypeDecl::module_path`]).
    pub module_path: String,
    /// The removed item's leaf name.
    pub name: String,
    /// Whether the removed item was a host *type* or a host *fn* — picks
    /// which boundary member a backend re-emits (an associated type vs. a
    /// trait method, on Rust).
    pub kind: DeprecatedHostKind,
    /// The frozen Kio′ declaration as it stood at its last
    /// `add` / `modify` origin — the source of the re-emitted signature.
    pub frozen: crate::ast::SigItem,
    /// The frozen declaration's explicit import dependencies. These are
    /// required to reconstruct exact type identities during re-emission.
    pub imports: Vec<crate::ast::Import>,
    /// The contract generation at which the item was removed — surfaced
    /// in the deprecation note / trap message.
    pub removed_at_version: u32,
}

/// Whether a [`DeprecatedHostItem`] was a host type or a host fn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeprecatedHostKind {
    Type,
    Fn,
}

/// One host type declaration.
///
/// Carries enough information for any backend's renderer to produce
/// its surface form: the declared name, the optional role
/// annotation, the type-parameter list (for roleless parametric host types;
/// shared semantic validation makes this empty whenever `role` is set),
/// and the set of
/// [`BoundRequirement`]s the type must satisfy at typed backends.
///
/// The `source` field is a borrow back into the package file's
/// declaration so renderers that need extra detail (e.g. the
/// declaration's span for diagnostics) can reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTypeDecl<'a> {
    pub name: &'a str,
    /// The slash-path of the module declaring this `host type` — drives
    /// the namespaced host boundary (rung 1).
    pub module_path: &'a str,
    pub role: Option<Role>,
    pub type_params: &'a [crate::ast::TypeParam],
    pub bounds: BoundRequirementSet,
    pub source: &'a HostType<Routed>,
}

/// Recover the exact declaring identity carried by a canonical Routed
/// host-type path. Bare paths are lexical type variables at this phase,
/// never a package-global host-type fallback.
pub(crate) fn routed_host_type_identity(ty: &Type<Routed>) -> Option<(String, &str)> {
    let Type::Path { segments, .. } = ty else {
        return None;
    };
    let (name, module) = segments.split_last()?;
    if module.is_empty() {
        return None;
    }
    let module = module
        .iter()
        .map(|segment| segment.as_str())
        .collect::<Vec<_>>()
        .join("/");
    Some((module, name.as_str()))
}

/// Resolve the role of the exact host-type declaration named by a
/// canonical Routed path.
pub(crate) fn exact_host_type_role(package: &Package<Routed>, ty: &Type<Routed>) -> Option<Role> {
    if !matches!(ty, Type::Path { args, .. } if args.is_empty()) {
        return None;
    }
    let (module_path, name) = routed_host_type_identity(ty)?;
    package
        .module(&module_path)?
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::HostType(host_type) if host_type.name == name => {
                host_type.role.map(|role| role.role)
            }
            _ => None,
        })
}

/// One env function declaration.
///
/// Borrows the entire `HostEnvFn<Routed>` so a renderer can read every
/// parameter and the return type in their full `Routed`-phase form. The Rust
/// backend joins this exact declaration identity to its prepared facade plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFnDecl<'a> {
    pub name: &'a str,
    /// The slash-path of the module declaring this `host fn` — drives
    /// the namespaced host boundary (rung 1).
    pub module_path: &'a str,
    pub source: &'a HostFn<Routed>,
}

/// What a typed backend needs each host type's bound surface to
/// commit to.
///
/// Each variant names the *purpose* of one bound, not the bound's
/// surface syntax. The Rust renderer translates a
/// `Clone + 'static + PartialEq` bound set into the appropriate
/// Rust trait bounds. The JS backend ignores every bound —
/// JS is dynamically typed and has no type-level obligations to
/// discharge.
///
/// The set's intent reflects how Kio's emitter uses the host type
/// export:
///
/// - **`CloneForRcCapture`** — the type can flow into a closure that captures
///   it by clone and stores the resulting closure in Rust's reference-counted
///   callable representation: `Rc<dyn Fn>` by default, or the corresponding
///   `Arc<dyn Fn + Send + Sync>` under the thread-safe build option.
/// - **`StaticForDynAnyStorage`** — the type can enter Rust's existential
///   type-erasure storage: `Rc<dyn Any>` by default, or
///   `Arc<dyn Any + Send + Sync>` under the thread-safe build option.
/// - **`PartialEqForShapeDerive`** — the minted structural shapes that
///   reference the host type derive `PartialEq`. Float roles relax
///   `Eq` to `PartialEq` (NaN is its own inequality), so the
///   descriptor commits only to the `PartialEq` half; renderers
///   decide whether to add `Eq` based on the role.
/// - **`CloneForPackageClone`** — every host type Kio emits requires
///   the package's outer wrapper type to be `Clone` for closure
///   self-capture; the host type therefore must be `Clone` too.
///
/// Renderers that produce untyped surfaces (JS) ignore the set
/// entirely; renderers that produce typed surfaces translate it
/// once and apply the result to every host-type signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundRequirement {
    CloneForRcCapture,
    StaticForDynAnyStorage,
    PartialEqForShapeDerive,
    CloneForPackageClone,
}

/// Insertion-order-preserving set of [`BoundRequirement`]s on one
/// host type.
///
/// Implemented over `Vec` rather than a `HashSet` so renderers get
/// deterministic iteration. The set is small (today: at most four
/// entries) so the linear-time `insert` is cheap and the order is
/// the one renderers want anyway (a `Clone + 'static + PartialEq`
/// bound surface is what every host today already sees).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoundRequirementSet {
    items: Vec<BoundRequirement>,
}

impl BoundRequirementSet {
    /// New, empty set.
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Add a bound requirement. No-op when the requirement is
    /// already present.
    pub fn insert(&mut self, req: BoundRequirement) {
        if !self.items.contains(&req) {
            self.items.push(req);
        }
    }

    /// Iterate in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = BoundRequirement> + '_ {
        self.items.iter().copied()
    }

    /// True when the requirement is present.
    pub fn contains(&self, req: BoundRequirement) -> bool {
        self.items.contains(&req)
    }

    /// Number of requirements in the set.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no requirements are present.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Walk a routed package file and produce a [`HostDescriptor`] over
/// its host items.
///
/// The walk is pure: a single linear pass over `export.env`,
/// preserving declaration order. Each `HostEnvType` populates its
/// `bounds` with the set of [`BoundRequirement`]s every Kio-emitted
/// typed-backend trait needs the host type to satisfy. Today every
/// host type gets the same uniform set (Kio's emitter uniformly captures host
/// values into its selected reference-counted erased slots, derives
/// `PartialEq` on the minted shapes, etc.); the per-host-type bound machinery is
/// in place so future emitter changes that need a per-type carve-out
/// (e.g., "this host type never flows through a closure boundary, so
/// drop `'static`") can populate the set per-declaration without
/// having to rewrite every renderer.
pub fn build_host_descriptor(package: &Package<Routed>) -> HostDescriptor<'_> {
    let mut host_types = Vec::new();
    let mut host_fns = Vec::new();
    let bridged = crate::pass::resolve::bridged_module_paths(package);
    // Scan bridged modules in module-path order for deterministic
    // output; each `host` item carries its declaring module path.
    for (path, entry) in package.modules() {
        if !bridged.contains(path) {
            continue;
        }
        for item in &entry.module.items {
            match item {
                Item::HostType(t) => host_types.push(decl_for_host_type(t, path)),
                Item::HostFn(f) => host_fns.push(decl_for_host_fn(f, path)),
                _ => {}
            }
        }
    }
    HostDescriptor {
        host_types,
        host_fns,
        contract_version: None,
        deprecated_host_items: Vec::new(),
    }
}

/// Fold a replayed signature changelog into a [`HostDescriptor`],
/// populating the contract version and the deprecated host items.
///
/// The base descriptor is the live `build_host_descriptor` walk; this
/// layers the sig-derived fields on top so a build that consumes a
/// signature file re-emits the removed host items. The
/// `removed`-recovery side (host vs export) comes from replay: only the
/// **env**-side removals (host types / host fns) become deprecated host
/// items — an export removal is not a host-source-stability concern.
pub fn with_signature<'a>(
    mut descriptor: HostDescriptor<'a>,
    version: u32,
    replayed: &crate::sig::ReplayedInterface,
) -> HostDescriptor<'a> {
    descriptor.contract_version = Some(version);
    descriptor.deprecated_host_items = replayed
        .removed
        .iter()
        .filter(|item| item.entry.side == crate::sig::ContractSide::Env)
        .filter_map(|item| {
            let kind = match &item.frozen {
                crate::ast::SigItem::HostType(_) => DeprecatedHostKind::Type,
                crate::ast::SigItem::HostFn(_) => DeprecatedHostKind::Fn,
                // An env-side removal is a host type or host fn by
                // construction (replay tags the side from the frozen
                // declaration). A non-host frozen item on the env side
                // cannot occur.
                _ => return None,
            };
            Some(DeprecatedHostItem {
                module_path: item.entry.name.module_path.clone(),
                name: item.entry.name.leaf.clone(),
                kind,
                frozen: item.frozen.clone(),
                imports: item.imports.clone(),
                removed_at_version: item.removed_at_version,
            })
        })
        .collect();
    descriptor
}

fn decl_for_host_type<'a>(t: &'a HostType<Routed>, module_path: &'a str) -> HostTypeDecl<'a> {
    HostTypeDecl {
        name: &t.name,
        module_path,
        role: t.role.map(|r| r.role),
        type_params: &t.type_params,
        bounds: default_bounds_for_host_type(),
        source: t,
    }
}

fn decl_for_host_fn<'a>(f: &'a HostFn<Routed>, module_path: &'a str) -> HostFnDecl<'a> {
    HostFnDecl {
        name: &f.name,
        module_path,
        source: f,
    }
}

/// The default bound set every host type gets today.
///
/// Kio's typed-backend emitters uniformly:
/// - Capture host values into closures (`CloneForRcCapture`).
/// - Store erased values in the selected reference-counted `dyn Any`
///   representation (`StaticForDynAnyStorage`).
/// - Derive `PartialEq` on minted shapes referencing the host type
///   (`PartialEqForShapeDerive`).
/// - Require the package wrapper to be `Clone` (`CloneForPackageClone`).
///
/// The requirements are a per-type set so an emitter pass that
/// proves a specific host type never flows through one of these
/// positions can drop the corresponding requirement without
/// touching the descriptor's shape.
fn default_bounds_for_host_type() -> BoundRequirementSet {
    let mut set = BoundRequirementSet::new();
    set.insert(BoundRequirement::CloneForPackageClone);
    set.insert(BoundRequirement::CloneForRcCapture);
    set.insert(BoundRequirement::StaticForDynAnyStorage);
    set.insert(BoundRequirement::PartialEqForShapeDerive);
    set
}

// ---- Serializable manifest projection ------------------------------------

/// Owned, serializable projection of a [`HostDescriptor`].
///
/// The descriptor itself borrows from an `PackageFile<Routed>`; that
/// shape is convenient for in-process renderers but unsuitable for
/// the test runner, which needs to read host-trait shape from a
/// file written by `kio build rust`. [`HostManifest`] is the file
/// shape: every field is owned and `serde`-(de)serializable.
///
/// The manifest carries less than the full descriptor — only what
/// the consumer of the file actually needs. The Rust test runner
/// today reads:
/// - Associated-type names and roles (to synthesize `StubHost`
///   type-decls and pick a native Rust type per role).
/// - Method signatures (to render canonical bodies via the shared
///   `canonical` classifier).
///
/// Renderers that need the full typed AST work off the in-process
/// [`HostDescriptor`] directly. The Rust trait emitter joins it to the
/// prepared facade catalog; the manifest is a contract with out-of-process
/// consumers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostManifest {
    /// Top-level host types.
    pub host_types: Vec<HostTypeManifest>,
    /// Top-level host fns.
    pub host_fns: Vec<HostFnManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostTypeManifest {
    pub name: String,
    /// The slash-path of the declaring module — the namespaced host
    /// boundary's key.
    #[serde(default)]
    pub module_path: String,
    /// `i32`, `str`, … verbatim. `None` when the declaration has no
    /// `role(...)` annotation; roleless host types remain ordinary exact
    /// host declarations.
    pub role: Option<String>,
    /// `Clone`, `Static`, `PartialEq`, `PackageClone` — the
    /// stringified [`BoundRequirement`] variants, in insertion
    /// order. Out-of-process consumers don't translate these (they
    /// see whatever surface bounds the per-backend renderer
    /// produced) but the manifest carries the intent so an audit
    /// can cross-check the rendered bounds against the
    /// descriptor's commitments.
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostFnManifest {
    pub name: String,
    /// The slash-path of the declaring module — the namespaced host
    /// boundary's key.
    #[serde(default)]
    pub module_path: String,
}

impl HostDescriptor<'_> {
    /// Project this descriptor into a [`HostManifest`].
    ///
    /// The manifest is the over-the-wire / on-disk shape the test
    /// runner consumes; the descriptor is the in-process shape that
    /// renderers consume. Both come from the same `build_host_descriptor`
    /// walk.
    pub fn to_manifest(&self) -> HostManifest {
        HostManifest {
            host_types: self
                .host_types
                .iter()
                .map(|t| HostTypeManifest {
                    name: t.name.to_owned(),
                    module_path: t.module_path.to_owned(),
                    role: t.role.map(|r| r.as_str().to_owned()),
                    bounds: t
                        .bounds
                        .iter()
                        .map(|b| bound_requirement_name(b).to_owned())
                        .collect(),
                })
                .collect(),
            host_fns: self
                .host_fns
                .iter()
                .map(|f| HostFnManifest {
                    name: f.name.to_owned(),
                    module_path: f.module_path.to_owned(),
                })
                .collect(),
        }
    }
}

/// Stable string name for one [`BoundRequirement`] variant.
///
/// Used by the manifest projection. Out-of-process consumers that
/// reason about the bound set compare these strings.
pub fn bound_requirement_name(b: BoundRequirement) -> &'static str {
    match b {
        BoundRequirement::CloneForRcCapture => "CloneForRcCapture",
        BoundRequirement::StaticForDynAnyStorage => "StaticForDynAnyStorage",
        BoundRequirement::PartialEqForShapeDerive => "PartialEqForShapeDerive",
        BoundRequirement::CloneForPackageClone => "CloneForPackageClone",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_requirement_set_preserves_insertion_order_and_dedupes() {
        let mut set = BoundRequirementSet::new();
        set.insert(BoundRequirement::CloneForRcCapture);
        set.insert(BoundRequirement::PartialEqForShapeDerive);
        set.insert(BoundRequirement::CloneForRcCapture); // dedup
        set.insert(BoundRequirement::StaticForDynAnyStorage);
        let v: Vec<_> = set.iter().collect();
        assert_eq!(
            v,
            vec![
                BoundRequirement::CloneForRcCapture,
                BoundRequirement::PartialEqForShapeDerive,
                BoundRequirement::StaticForDynAnyStorage,
            ]
        );
        assert_eq!(set.len(), 3);
        assert!(set.contains(BoundRequirement::CloneForRcCapture));
        assert!(!set.contains(BoundRequirement::CloneForPackageClone));
    }

    #[test]
    fn bound_requirement_names_are_stable() {
        // These string names cross the on-disk JSON boundary in the
        // host-trait manifest, so they're a contract. If a name
        // changes, the test runner that reads the manifest needs to
        // update in the same change.
        assert_eq!(
            bound_requirement_name(BoundRequirement::CloneForRcCapture),
            "CloneForRcCapture",
        );
        assert_eq!(
            bound_requirement_name(BoundRequirement::StaticForDynAnyStorage),
            "StaticForDynAnyStorage",
        );
        assert_eq!(
            bound_requirement_name(BoundRequirement::PartialEqForShapeDerive),
            "PartialEqForShapeDerive",
        );
        assert_eq!(
            bound_requirement_name(BoundRequirement::CloneForPackageClone),
            "CloneForPackageClone",
        );
    }

    #[test]
    fn default_bounds_cover_every_emitter_position() {
        // Today every host type gets the full set of bound
        // requirements; this test pins that until a per-type
        // analysis lets the emitter drop bounds it can prove
        // unnecessary.
        let bounds = default_bounds_for_host_type();
        assert!(bounds.contains(BoundRequirement::CloneForRcCapture));
        assert!(bounds.contains(BoundRequirement::StaticForDynAnyStorage));
        assert!(bounds.contains(BoundRequirement::PartialEqForShapeDerive));
        assert!(bounds.contains(BoundRequirement::CloneForPackageClone));
        assert_eq!(bounds.len(), 4);
    }
}

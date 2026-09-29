//! `kio sig` Phase 2 — the contract-surface compatibility checker.
//!
//! This module is the backend-independent core that Phase 3 (the
//! `kio sig` CLI) and Phase 4 (the deprecated-re-emit emitter + the
//! artifact-cache key) build on. It does three things:
//!
//! 1. **Snapshot** ([`ContractSnapshot::from_package`]) — project a
//!    typechecked package's *contract surface* into a backend-independent,
//!    module-qualified record: the `pub host` items are the **env**
//!    (contravariant requirements), the other `pub` items of bridged
//!    modules are the **exports** (covariant provisions), plus the
//!    transitive type closure of every type reachable from those
//!    signatures.
//!
//! 2. **Replay** ([`replay`]) — reconstruct the *current interface* a
//!    `*.sig.kio` changelog describes by applying its version blocks in
//!    order (`add` → present, `modify` → reshape, `remove` → drop),
//!    recovering a removed item's side (host vs export) and frozen
//!    signature from its earlier `add` / `modify` origin.
//!
//! 3. **Compare** ([`compare`]) — the two-sided opposite-variance
//!    verdict (`super::verdict`): export side covariant, env side
//!    contravariant, v1 strict alpha+kind-equality, `role(...)` part of
//!    host-type identity, and a closure re-check so a removal that
//!    strands a still-referenced type is breaking.
//!
//! Identity is **module-qualified** (`a.main` ≠ `b.main`): every entry
//! is keyed by [`QualifiedName`]. The snapshot is computed from the
//! *typechecked* package — resolved references, no network or build
//! output — so it is a pure function of the package's typed AST.

mod digest;
mod draft;
mod emit;
mod normalize;
mod record;
mod replay;
mod validate;
mod verdict;

pub use digest::contract_digest;
pub use draft::{DraftPlan, compute_draft, replay_through};
#[cfg(all(feature = "cli", feature = "surface"))]
pub(crate) use emit::emit_signature_comparison;
pub use emit::emit_signature_file;
pub use record::{RecordedItem, RecordedSurface};
#[cfg(all(feature = "cli", feature = "surface"))]
pub(crate) use replay::FrozenTypeDeclaration;
#[cfg(test)]
pub(crate) use replay::replay_unvalidated_for_downstream_defense;
pub(crate) use replay::{FrozenTypeClosure, FrozenTypeItem};
pub use replay::{RemovedItem, ReplayError, ReplayedInterface, replay};
pub use verdict::{Change, ChangePlacement, CompatReport, Verdict, compare};

use crate::ast::{
    Item, Meta, Newtype, NewtypeHostSurface, Phase, Purity, SigItem, Surface, Type, TypeRecGroup,
    TypeRecMember, Visibility,
};
use crate::pass::resolve::{ExportContractPhase, Package};
use std::collections::BTreeMap;

/// A module-qualified contract-item name: `(module_path, leaf)`. The
/// `module_path` is `/`-joined (`a/b`); the `leaf` is the item's
/// declared name. `a.main` and `b.main` are distinct keys — identity is
/// module-qualified, per the design.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct QualifiedName {
    pub module_path: String,
    pub leaf: String,
}

impl QualifiedName {
    pub fn new(module_path: impl Into<String>, leaf: impl Into<String>) -> Self {
        Self {
            module_path: module_path.into(),
            leaf: leaf.into(),
        }
    }
}

impl std::fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.module_path, self.leaf)
    }
}

/// Which side of the contract an item sits on.
///
/// The env side is **contravariant** (host requirements: adding one
/// breaks, removing one is compatible at the language level); the export
/// side is **covariant** (provisions: adding one is compatible, removing
/// one breaks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContractSide {
    /// A `pub host` item — a requirement the host satisfies.
    Env,
    /// A non-host `pub` item — a provision a host invokes.
    Export,
}

/// One entry in a [`ContractSnapshot`]: a single contract item, in a
/// backend-independent comparable form.
///
/// The `signature` string is the canonical alpha+kind-normalized form
/// used by the verdict comparison; it is also exactly what a removed
/// item's deprecation re-emit recovers (Phase 4 reads `kind` + the
/// recorded raw declaration carried alongside, not stored here, since
/// replay keeps the originating AST). For comparison, the snapshot only
/// needs the side, kind, role, and normalized signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEntry {
    pub name: QualifiedName,
    pub side: ContractSide,
    pub kind: ContractKind,
}

/// The per-kind comparable shape of a contract item.
///
/// Per the design's **per-kind type-identity** rules: host `type` is
/// opaque (qualified name + params + role); a transparent alias is its
/// one-level, head-qualified normalized body (nested alias refs stay
/// nominal — the normalizer does not recursively expand them); a
/// `newtype` is nominal (qualified name + kind/arity, payload only via
/// `pub` members); host/export fns compare by their normalized signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractKind {
    /// `host type Foo[A];` or `host type Foo role(i32);` — opaque. Identity is the
    /// qualified name (the [`ContractEntry::name`]) plus the
    /// type-parameter kinds and the role. `role(...)` is part of
    /// host-type identity.
    HostType {
        /// The binder kinds, in order. This is empty whenever `role` is set.
        param_kinds: Vec<String>,
        /// The `role(...)` annotation as its surface string (`i32`,
        /// `str`, …), or `None` for a role-less host type.
        role: Option<String>,
    },
    /// A host fn (env) or a body-less export fn (export). Identity is
    /// the normalized signature; an export also records whether callers
    /// may use it during compile-time evaluation.
    Fn {
        /// The canonical alpha+kind-normalized function type.
        signature: String,
        /// `true` only for an ordinary exported function declared
        /// `pure`. Host functions always set this to `false`.
        pure: bool,
    },
    /// `type Name = …;` — a transparent alias. Identity is the
    /// one-level, head-qualified normalized body (nested alias refs stay
    /// nominal — the normalizer does not recursively expand them).
    Alias {
        /// The binder kinds, in order.
        param_kinds: Vec<String>,
        /// The canonical normalized expansion (the alias body).
        expansion: String,
    },
    /// `newtype Name = …;` — nominal. Identity is the kind/arity plus
    /// the `pub` member surface; the payload matters only through a
    /// `pub` member.
    Newtype {
        /// The binder kinds, in order.
        param_kinds: Vec<String>,
        /// Exactly the public member/payload surface. Private and scoped
        /// members leave no name or payload behind in the contract.
        surface: PublicNewtypeSurface,
    },
}

/// The public surface of a nominal newtype. Keeping the constructor and
/// projector as distinct variants preserves their semantic roles without
/// retaining placeholder entries for hidden members. The opaque variant
/// cannot carry a payload, making hidden payload leakage unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicNewtypeSurface {
    Opaque,
    Constructor {
        name: String,
        payload: String,
    },
    Projector {
        name: String,
        payload: String,
    },
    ConstructorAndProjector {
        constructor: String,
        projector: String,
        payload: String,
    },
}

impl PublicNewtypeSurface {
    pub(crate) fn from_host_surface<P: Phase>(
        surface: Option<NewtypeHostSurface<'_, P>>,
        payload: impl FnOnce(&Type<P>) -> String,
    ) -> Self {
        match surface {
            None | Some(NewtypeHostSurface::Opaque) => Self::Opaque,
            Some(NewtypeHostSurface::Constructor {
                constructor,
                payload: payload_type,
            }) => Self::Constructor {
                name: constructor.name.clone(),
                payload: payload(payload_type),
            },
            Some(NewtypeHostSurface::Projector {
                projector,
                payload: payload_type,
            }) => Self::Projector {
                name: projector.name.clone(),
                payload: payload(payload_type),
            },
            Some(NewtypeHostSurface::Both {
                constructor,
                projector,
                payload: payload_type,
            }) => Self::ConstructorAndProjector {
                constructor: constructor.name.clone(),
                projector: projector.name.clone(),
                payload: payload(payload_type),
            },
        }
    }

    pub(crate) fn payload(&self) -> Option<&str> {
        match self {
            Self::Opaque => None,
            Self::Constructor { payload, .. }
            | Self::Projector { payload, .. }
            | Self::ConstructorAndProjector { payload, .. } => Some(payload),
        }
    }

    pub(crate) fn member_names(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Opaque => (None, None),
            Self::Constructor { name, .. } => (Some(name), None),
            Self::Projector { name, .. } => (None, Some(name)),
            Self::ConstructorAndProjector {
                constructor,
                projector,
                ..
            } => (Some(constructor), Some(projector)),
        }
    }
}

/// A backend-independent, module-qualified snapshot of a package's
/// contract surface. Built by [`ContractSnapshot::from_package`] from a
/// typechecked package, or by [`replay`] from a parsed signature
/// changelog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContractSnapshot {
    /// Every contract item, keyed by module-qualified name.
    pub items: BTreeMap<QualifiedName, ContractEntry>,
}

impl ContractSnapshot {
    /// Build the contract-surface snapshot of a typechecked package.
    ///
    /// Walks the bridged modules (selected by the package `bridge { … }`
    /// globs); from each, every `pub` item contributes an entry —
    /// `pub host` items on the env side, other `pub` items on the export
    /// side. Each signature type is qualified to its fully
    /// module-qualified contract form (via the resolver's
    /// `exported_contract_type`) and then canonicalized, so the
    /// snapshot's normalized forms line up with a replayed sig's.
    ///
    /// The phase is a typechecked phase that still retains ordinary-function
    /// purity. The [`ExportContractPhase`] bound is what
    /// `exported_contract_type` needs (surface-only variants statically
    /// uninhabited); the associated-type equality prevents a post-Prime phase
    /// from silently projecting an erased purity marker as `impure`.
    ///
    /// A Prime package retains the marker and is accepted:
    ///
    /// ```no_run
    /// use kio_lang::ast::Prime;
    /// use kio_lang::pass::resolve::Package;
    /// use kio_lang::sig::ContractSnapshot;
    ///
    /// fn project(package: &Package<Prime>) {
    ///     let _ = ContractSnapshot::from_package(package);
    /// }
    /// ```
    ///
    /// A post-Prime package is rejected at compile time:
    ///
    /// ```compile_fail
    /// use kio_lang::ast::Routed;
    /// use kio_lang::pass::resolve::Package;
    /// use kio_lang::sig::ContractSnapshot;
    ///
    /// fn project(package: &Package<Routed>) {
    ///     let _ = ContractSnapshot::from_package(package);
    /// }
    /// ```
    pub fn from_package<P>(package: &Package<P>) -> Self
    where
        P: ExportContractPhase + Phase<FnPurity = Purity> + Clone,
    {
        surface::snapshot_from_package(package)
    }
}

mod surface;

/// The canonical normalized form of a type, qualified via `qualify_head`.
/// Re-exported for the snapshot builder and the replay path so both
/// sides produce identical strings for alpha+kind-equal types.
pub(crate) fn canonical_type<P: Phase>(
    ty: &Type<P>,
    qualify_head: &impl Fn(&[String]) -> Vec<String>,
) -> String {
    normalize::canonical(ty, qualify_head)
}

pub(crate) fn canonical_declaration_type<P: Phase>(
    ty: &Type<P>,
    universals: &[crate::ast::TypeParam],
    existentials: &[crate::ast::TypeParam],
    qualify_head: &impl Fn(&[String]) -> Vec<String>,
) -> String {
    normalize::canonical_declaration(ty, universals, existentials, qualify_head)
}

/// Canonical ordinary-Kio declaration used when a public newtype crosses the
/// signature boundary. The grammar always requires both members and a
/// payload, so facts absent from the public surface are represented by
/// deterministic private member names and, for a fully opaque newtype, unit.
pub(crate) fn project_newtype_declaration(mut newtype: Newtype<Surface>) -> Newtype<Surface> {
    // The caller has already selected this newtype as a contract entry.
    // Signature sections do not require the redundant outer `pub` spelling,
    // so establish their implied visibility before deriving the member surface.
    newtype.vis = Visibility::Public;
    let (constructor_public, projector_public) = match newtype.host_surface() {
        Some(NewtypeHostSurface::Opaque) => (false, false),
        Some(NewtypeHostSurface::Constructor { .. }) => (true, false),
        Some(NewtypeHostSurface::Projector { .. }) => (false, true),
        Some(NewtypeHostSurface::Both { .. }) => (true, true),
        None => unreachable!("a projected signature newtype has public outer visibility"),
    };
    newtype.doc = None;

    let mut retained = std::collections::BTreeSet::new();
    if constructor_public {
        retained.insert(newtype.constructor.name.clone());
    }
    if projector_public {
        retained.insert(newtype.projector.name.clone());
    }

    if constructor_public {
        newtype.constructor.vis = Visibility::Public;
    } else {
        newtype.constructor.vis = Visibility::Private;
        newtype.constructor.name = inert_member_name("constructor", &retained);
        newtype.constructor.leading_trivia.clear();
        retained.insert(newtype.constructor.name.clone());
    }
    if projector_public {
        newtype.projector.vis = Visibility::Public;
    } else {
        newtype.projector.vis = Visibility::Private;
        newtype.projector.name = inert_member_name("projector", &retained);
        newtype.projector.leading_trivia.clear();
    }

    if !constructor_public && !projector_public {
        newtype.existential_params.clear();
        newtype.payload = Type::Unit {
            meta: Meta::new(newtype.payload.span()),
        };
        // Erasing the payload also erases every possible self-edge. Keeping a
        // singleton marker here would make the projected declaration invalid
        // (`rec` is required only for an actual recursive dependency).
        newtype.rec_span = None;
        newtype.meta.trailing_trivia.clear();
    }
    newtype
}

/// Project one signature declaration onto its public contract. A parsed
/// signature keeps its written recursive container so ordinary fresh-artifact
/// validation, rather than a projection repair, owns that container's shape.
pub(crate) fn project_signature_item(item: &SigItem<Surface>) -> SigItem<Surface> {
    match item {
        SigItem::Newtype(newtype) => SigItem::Newtype(project_newtype_declaration(newtype.clone())),
        SigItem::TypeRecGroup(group) => {
            let mut group = group.clone();
            for member in &mut group.members {
                if let TypeRecMember::Newtype(newtype) = member {
                    *newtype = project_newtype_declaration(newtype.clone());
                }
            }
            SigItem::TypeRecGroup(group)
        }
        _ => item.clone(),
    }
}

/// Repartition one already-validated live package group after its public
/// projection removes hidden payload edges. Unlike parsed signature input,
/// this starts from a group whose written shape the ordinary compiler already
/// validated. The result is dependency ordered: acyclic members are ordinary
/// items, self-recursive newtypes use `rec newtype`, and only mutual SCCs stay
/// grouped.
pub(crate) fn repartition_recorded_type_rec_group(
    group: TypeRecGroup<Surface>,
) -> Vec<SigItem<Surface>> {
    let edges = projected_type_rec_edges(&group.members);
    let components = crate::pass::resolve::strongly_connected_components(&edges, |_| true);
    let cyclic_components = components
        .iter()
        .filter(|component| component.len() > 1 || edges[component[0]].contains(&component[0]))
        .cloned()
        .collect();
    let analysis = crate::pass::resolve::TypeRecAnalysis {
        edge_spans: edges
            .iter()
            .map(|targets| {
                targets
                    .iter()
                    .map(|target| (*target, crate::span::Span::new(0, 0)))
                    .collect()
            })
            .collect(),
        edges,
        components,
        cyclic_components,
        // The emitter preserves an invalid alias-only singleton/group for the
        // normal signature validator; it needs only the component partition.
        alias_cycle: None,
    };
    crate::pass::resolve::emit_type_rec_partition(group, &analysis)
        .into_iter()
        .map(|item| match item {
            Item::TypeAlias(alias) => SigItem::TypeAlias(alias),
            Item::Newtype(newtype) => SigItem::Newtype(newtype),
            Item::TypeRecGroup(group) => SigItem::TypeRecGroup(group),
            Item::Labels(_, _) => {
                unreachable!("signature recursive contexts contain no Surface labels")
            }
            _ => unreachable!("a type-recursive partition emits only type declarations"),
        })
        .collect()
}

fn projected_type_rec_member_name(member: &TypeRecMember<Surface>) -> Option<&str> {
    match member {
        TypeRecMember::TypeAlias(alias) => Some(&alias.name),
        TypeRecMember::Newtype(newtype) => Some(&newtype.name),
        TypeRecMember::Labels(_, _) => None,
    }
}

fn projected_type_rec_edges(members: &[TypeRecMember<Surface>]) -> Vec<Vec<usize>> {
    let names = members
        .iter()
        .enumerate()
        .filter_map(|(index, member)| {
            projected_type_rec_member_name(member).map(|name| (name, index))
        })
        .collect::<std::collections::HashMap<_, _>>();
    members
        .iter()
        .map(|member| {
            let (ty, mut bound) = match member {
                TypeRecMember::TypeAlias(alias) => (
                    &alias.body,
                    alias
                        .type_params
                        .iter()
                        .map(|param| param.name.as_str())
                        .collect::<Vec<_>>(),
                ),
                TypeRecMember::Newtype(newtype) => (
                    &newtype.payload,
                    newtype
                        .type_params
                        .iter()
                        .chain(&newtype.existential_params)
                        .map(|param| param.name.as_str())
                        .collect::<Vec<_>>(),
                ),
                TypeRecMember::Labels(_, _) => return Vec::new(),
            };
            let mut out = Vec::new();
            collect_projected_type_rec_edges(ty, &names, &mut bound, &mut out);
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect()
}

fn collect_projected_type_rec_edges<'a>(
    ty: &'a Type<Surface>,
    names: &std::collections::HashMap<&'a str, usize>,
    bound: &mut Vec<&'a str>,
    out: &mut Vec<usize>,
) {
    match ty {
        Type::Path { segments, args, .. } => {
            if let [head] = segments.as_slice()
                && !bound.contains(&head.as_str())
                && let Some(index) = names.get(head.as_str())
            {
                out.push(*index);
            }
            for argument in args {
                collect_projected_type_rec_edges(argument, names, bound, out);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_projected_type_rec_edges(param, names, bound, out);
            collect_projected_type_rec_edges(ret, names, bound, out);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_projected_type_rec_edges(left, names, bound, out);
            collect_projected_type_rec_edges(right, names, bound, out);
        }
        Type::Forall { param, body, .. } => {
            bound.push(param.name.as_str());
            collect_projected_type_rec_edges(body, names, bound, out);
            bound.pop();
        }
        Type::Goal { args, .. } => {
            for argument in args {
                collect_projected_type_rec_edges(argument, names, bound, out);
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } | Type::LabelSugar { .. } => {}
    }
}

fn inert_member_name(role: &str, retained: &std::collections::BTreeSet<String>) -> String {
    let stem = format!("sig_private_{role}");
    if !retained.contains(&stem) {
        return stem;
    }
    for suffix in 1_u64.. {
        let candidate = format!("{stem}_n{suffix}");
        if !retained.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unbounded deterministic member-name sequence cannot be exhausted")
}

#[cfg(all(test, feature = "surface"))]
mod integration_tests {
    use super::*;
    use crate::ast::{Meta, PackageFile, Prime, SigItem, Surface};
    use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry, TopLevelScope};
    use crate::span::Span;
    use std::path::PathBuf;

    /// Lower one module source through surface → Prime and assemble a
    /// `Package<Prime>` with the given package-file source (for its
    /// `bridge` block). Mirrors the `capabilities` test pipeline.
    fn typed_package(module_path: &str, module_src: &str, pkg_src: &str) -> Package<Prime> {
        let module: crate::ast::Module<Surface> =
            crate::pass::parser::parse(module_src).expect("parse module");
        let module = crate::pass::desugar::desugar_module(module).expect("desugar");
        let (lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from(format!("{module_path}.kio")), module)],
            None,
        )
        .expect("elaborate");
        let lowered_module = lowered.into_iter().next().expect("one module").1;
        let elabs = crate::pass::typecheck_full::Elaborations::new();
        let normalized = crate::pass::alpha_normalize::normalize_module(&lowered_module);
        let prime = crate::pass::substitute::substitute_module(normalized.module(), &elabs);
        let scope = TopLevelScope::build(&prime).expect("scope");

        // The bridge block is phase-independent; lift it off a parsed
        // surface package file into a `PackageFile<Prime>`.
        let surface_pkg =
            crate::pass::parser::parse_package_file(pkg_src, Some("app")).expect("parse pkg");
        let pkg_prime = PackageFile::<Prime> {
            name: surface_pkg.name,
            build: surface_pkg.build,
            bridge: surface_pkg.bridge,
            meta: Meta::new(Span::new(0, 0)),
        };
        Package::<Prime>::from_parts(
            std::iter::once((
                module_path.to_owned(),
                ModuleEntry::<Prime> {
                    file_path: PathBuf::from(format!("{module_path}.kio")),
                    module: prime,
                    scope,
                },
            ))
            .collect(),
            Some(PackageFileEntry {
                file_path: PathBuf::from("app.pkg.kio"),
                package_name: "app".to_owned(),
                package_file: pkg_prime,
            }),
        )
    }

    fn lowered_package(
        module_path: &str,
        module_src: &str,
        pkg_src: &str,
    ) -> Package<crate::ast::Lowered> {
        let module: crate::ast::Module<Surface> =
            crate::pass::parser::parse(module_src).expect("parse module");
        let module = crate::pass::desugar::desugar_module(module).expect("desugar");
        let (lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from(format!("{module_path}.kio")), module)],
            None,
        )
        .expect("elaborate");
        let lowered_module = lowered.into_iter().next().expect("one module").1;
        let scope = TopLevelScope::build(&lowered_module).expect("scope");
        let surface_pkg =
            crate::pass::parser::parse_package_file(pkg_src, Some("app")).expect("parse pkg");
        let package_file = PackageFile::<crate::ast::Lowered> {
            name: surface_pkg.name,
            build: surface_pkg.build,
            bridge: surface_pkg.bridge,
            meta: Meta::new(Span::new(0, 0)),
        };
        Package::<crate::ast::Lowered>::from_parts(
            std::iter::once((
                module_path.to_owned(),
                ModuleEntry::<crate::ast::Lowered> {
                    file_path: PathBuf::from(format!("{module_path}.kio")),
                    module: lowered_module,
                    scope,
                },
            ))
            .collect(),
            Some(PackageFileEntry {
                file_path: PathBuf::from("app.pkg.kio"),
                package_name: "app".to_owned(),
                package_file,
            }),
        )
    }

    #[test]
    fn live_snapshot_matches_replayed_sig() {
        let module_src = "\
module api;

host type Handle role(i32);

host fn open() -> Handle;

pub fn serve(h: Handle) -> Handle { h }
";
        let pkg_src = "package app;\n\nbridge {\n  api;\n}\n";
        let package = typed_package("api", module_src, pkg_src);
        let live = ContractSnapshot::from_package(&package);

        // The live surface has the three bridged `pub` items.
        assert!(
            live.items
                .contains_key(&QualifiedName::new("api", "Handle"))
        );
        assert!(live.items.contains_key(&QualifiedName::new("api", "open")));
        assert!(live.items.contains_key(&QualifiedName::new("api", "serve")));
        assert_eq!(
            live.items[&QualifiedName::new("api", "Handle")].side,
            ContractSide::Env
        );
        assert_eq!(
            live.items[&QualifiedName::new("api", "serve")].side,
            ContractSide::Export
        );

        // A sig changelog that records exactly this surface, replayed,
        // must compare clean against the live package — the snapshot and
        // the replay produce identical canonical forms.
        let sig_src = "\
signature app v(1);

v(1) {
  nonbreaking {
    add {
      module api {
        host type Handle role(i32);
        host fn open() -> Handle;
        pub fn serve(h: Handle) -> Handle;
      }
    }
  }
}
";
        let file =
            crate::pass::parser::parse_signature_file(sig_src, Some("app")).expect("parse sig");
        let replayed = replay(&file).expect("replay");
        let report = compare(&replayed.current, &live);
        assert!(
            report.is_empty(),
            "live surface and matching sig must agree: {report:?}",
        );
    }

    #[test]
    fn draft_records_a_projected_recursive_group_once_and_refs_its_members() {
        let package = typed_package(
            "api",
            r#"module api;

rec {
  pub type A = B;
  pub newtype B : A { pub constructor mk_b; pub projector un_b; };
}
"#,
            "package app;\n\nbridge {\n  api;\n}\n",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = record::RecordedSurface::from_package(&package);
        let plan = draft::compute_draft("app", None, &recorded, &live).expect("draft");
        let text = emit_signature_file(&plan.recomputed);
        assert_eq!(text.matches("rec {").count(), 1, "{text}");
        assert!(
            text.contains("with {\n    module api {\n      rec {"),
            "{text}"
        );
        assert!(
            text.contains("add {\n      api.A;\n      api.B\n    }"),
            "{text}"
        );
        assert!(!text.contains("module api {\n        pub type A"), "{text}");

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("emitted recursive sig parses");
        let replayed = replay(&parsed).expect("emitted recursive sig replays");
        assert!(compare(&replayed.current, &live).is_empty());
    }

    #[test]
    fn draft_records_a_projected_recursive_singleton_once_and_refs_it() {
        let package = typed_package(
            "api",
            r#"module api;

pub rec newtype List[A] : . | (A & List(A)) {
  pub constructor mk_list;
  pub projector un_list;
};
"#,
            "package app;\n\nbridge {\n  api;\n}\n",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = record::RecordedSurface::from_package(&package);
        let plan = draft::compute_draft("app", None, &recorded, &live).expect("draft");
        let text = emit_signature_file(&plan.recomputed);
        assert_eq!(text.matches("rec newtype List").count(), 1, "{text}");
        assert!(
            text.contains("with {\n    module api {\n      pub rec newtype List"),
            "{text}"
        );
        assert!(text.contains("add {\n      api.List\n    }"), "{text}");

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("emitted recursive singleton sig parses");
        let replayed = replay(&parsed).expect("emitted recursive singleton sig replays");
        assert!(compare(&replayed.current, &live).is_empty());
    }

    #[test]
    fn draft_rejects_an_invalid_fresh_signature_artifact() {
        let package = typed_package(
            "api",
            r#"module api;

pub rec newtype Bad : . | Bad {
  pub constructor mk;
  pub projector un;
};
"#,
            "package app; bridge { api; }",
        );
        let live = ContractSnapshot::from_package(&package);
        let mut recorded = record::RecordedSurface::from_package(&package);
        let malformed: crate::ast::Module<Surface> = crate::pass::parser::parse(
            r#"module api;
pub rec newtype Bad : (Bad -> .) {
  pub constructor mk;
  pub projector un;
};
"#,
        )
        .expect("parse malformed recorded declaration");
        let crate::ast::Item::Newtype(malformed) =
            malformed.items.into_iter().next().expect("one declaration")
        else {
            panic!("fixture must contain a newtype")
        };
        let name = QualifiedName::new("api", "Bad");
        recorded
            .items
            .get_mut(&name)
            .expect("recorded singleton")
            .item = SigItem::Newtype(malformed.clone());
        recorded
            .recursive_contexts
            .get_mut(&name)
            .expect("recursive singleton context")
            .section
            .items = vec![SigItem::Newtype(malformed)];

        let error = match draft::compute_draft("app", None, &recorded, &live) {
            Err(error) => error,
            Ok(_) => panic!("fresh draft production accepted its invalid reconstruction"),
        };
        assert!(error.diag().1.contains("strictly positive"), "{error:?}");
    }

    #[test]
    fn contract_projection_drops_a_hidden_recursive_singleton_edge() {
        let package = typed_package(
            "api",
            r#"module api;

pub rec newtype Secret : Secret {
  constructor mk_secret;
  projector un_secret;
};
"#,
            "package app;\n\nbridge {\n  api;\n}\n",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = record::RecordedSurface::from_package(&package);
        let plan = draft::compute_draft("app", None, &recorded, &live).expect("draft");
        let text = emit_signature_file(&plan.recomputed);
        assert!(!text.contains("with {"), "{text}");
        assert!(!text.contains("rec newtype"), "{text}");
        assert!(text.contains("pub newtype Secret : ."), "{text}");

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("projected recursive singleton sig parses");
        let replayed = replay(&parsed).expect("projected recursive singleton sig replays");
        assert!(compare(&replayed.current, &live).is_empty());
    }

    #[test]
    fn contract_projection_recomputes_and_drops_a_hidden_recursive_edge() {
        let package = typed_package(
            "api",
            r#"module api;

rec {
  pub newtype A : B { constructor mk_a; projector un_a; };
  pub newtype B : A { pub constructor mk_b; pub projector un_b; };
}
"#,
            "package app;\n\nbridge {\n  api;\n}\n",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = record::RecordedSurface::from_package(&package);
        let plan = draft::compute_draft("app", None, &recorded, &live).expect("draft");
        let text = emit_signature_file(&plan.recomputed);
        assert!(
            !text.contains("with {"),
            "opaque projection broke the SCC: {text}"
        );
        assert!(
            !text.contains("rec {"),
            "opaque projection broke the SCC: {text}"
        );
        assert!(text.contains("pub newtype A : ."), "{text}");
        assert!(text.contains("pub newtype B : A"), "{text}");

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("projected sig parses");
        let replayed = replay(&parsed).expect("projected sig replays");
        assert!(compare(&replayed.current, &live).is_empty());
    }

    #[test]
    fn contract_projection_repartitions_a_mutual_group_into_a_recursive_singleton() {
        let package = typed_package(
            "api",
            r#"module api;

rec {
  pub newtype A : A | B {
    pub constructor make_a;
    projector un_a;
  };
  pub newtype B : A {
    constructor make_b;
    projector un_b;
  };
}
"#,
            "package app; bridge { api; }",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = record::RecordedSurface::from_package(&package);
        let plan = draft::compute_draft("app", None, &recorded, &live)
            .expect("a projected self component has a standalone recursive context");
        let text = emit_signature_file(&plan.recomputed);
        assert!(text.contains("pub rec newtype A : A | B"), "{text}");
        assert!(!text.contains("rec {"), "{text}");
        assert!(text.contains("pub newtype B : ."), "{text}");
        assert!(text.contains("add {\n      api.A;"), "{text}");

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("repartitioned signature parses");
        let replayed = replay(&parsed).expect("repartitioned signature replays");
        assert!(compare(&replayed.current, &live).is_empty());
    }

    #[test]
    fn draft_carries_unchanged_peer_group_when_one_recursive_scc_splits() {
        let base = typed_package(
            "api",
            r#"module api;

rec {
  pub type A = B;
  pub newtype B : A | C { pub constructor mk_b; pub projector un_b; };
  pub type C = D;
  pub newtype D : C | A { pub constructor mk_d; pub projector un_d; };
}
"#,
            "package app; bridge { api; }",
        );
        let base_live = ContractSnapshot::from_package(&base);
        let base_recorded = record::RecordedSurface::from_package(&base);
        let mut existing = draft::compute_draft("app", None, &base_recorded, &base_live)
            .expect("initial draft")
            .recomputed;
        existing.version = 2; // seal v(1), leaving v(2) as the new draft

        let split = typed_package(
            "api",
            r#"module api;

rec {
  pub type A = B;
  pub newtype B : A | C { pub constructor mk_b; pub projector un_b; };
}

rec {
  pub type C = D;
  pub newtype D : C { pub constructor mk_d; pub projector un_d; };
}
"#,
            "package app; bridge { api; }",
        );
        let split_live = ContractSnapshot::from_package(&split);
        let split_recorded = record::RecordedSurface::from_package(&split);
        let plan = draft::compute_draft("app", Some(&existing), &split_recorded, &split_live)
            .expect("split draft");
        assert_eq!(plan.report.changes.len(), 1, "{:?}", plan.report);
        assert_eq!(plan.report.changes[0].name, QualifiedName::new("api", "D"));

        let text = emit_signature_file(&plan.recomputed);
        assert_eq!(text.matches("      rec {").count(), 3, "{text}");
        let v2 = text.rsplit_once("v(2) {").expect("v2 block").1;
        assert_eq!(v2.matches("      rec {").count(), 2, "{text}");
        assert!(v2.contains("modify {\n      api.D\n    }"), "{text}");
        assert!(
            !v2.lines()
                .any(|line| line.trim().trim_end_matches(';') == "api.A"),
            "unchanged split peer is context only: {text}"
        );

        let replayed = replay(&plan.recomputed).expect("split draft replays");
        assert!(compare(&replayed.current, &split_live).is_empty());
        let context_len = |leaf: &str| {
            let item = replayed
                .live_frozen
                .iter()
                .find(|item| item.entry.name == QualifiedName::new("api", leaf))
                .expect("live item");
            let context = item.recursive_context.as_ref().expect("recursive context");
            let SigItem::TypeRecGroup(group) = &context.items[0] else {
                panic!("expected recursive group context")
            };
            group.members.len()
        };
        assert_eq!(context_len("A"), 2);
        assert_eq!(context_len("B"), 2);
        assert_eq!(context_len("C"), 2);
        assert_eq!(context_len("D"), 2);
    }

    #[test]
    fn draft_clears_old_context_for_unchanged_peers_that_become_acyclic() {
        let base = typed_package(
            "api",
            r#"module api;

rec {
  pub type A = B;
  pub newtype B : C { pub constructor mk_b; pub projector un_b; };
  pub type C = A;
}
"#,
            "package app; bridge { api; }",
        );
        let base_live = ContractSnapshot::from_package(&base);
        let base_recorded = record::RecordedSurface::from_package(&base);
        let mut existing = draft::compute_draft("app", None, &base_recorded, &base_live)
            .expect("initial draft")
            .recomputed;
        existing.version = 2;

        let acyclic = typed_package(
            "api",
            r#"module api;

pub type C = .;
pub newtype B : C { pub constructor mk_b; pub projector un_b; };
pub type A = B;
"#,
            "package app; bridge { api; }",
        );
        let acyclic_live = ContractSnapshot::from_package(&acyclic);
        let acyclic_recorded = record::RecordedSurface::from_package(&acyclic);
        let plan = draft::compute_draft("app", Some(&existing), &acyclic_recorded, &acyclic_live)
            .expect("acyclic draft");
        assert_eq!(plan.report.changes.len(), 1, "{:?}", plan.report);
        assert_eq!(plan.report.changes[0].name, QualifiedName::new("api", "C"));

        let text = emit_signature_file(&plan.recomputed);
        let v2 = text.rsplit_once("v(2) {").expect("v2 block").1;
        assert!(!v2.contains("rec {"), "{text}");
        assert!(v2.contains("with {\n    module api {"), "{text}");
        assert!(v2.contains("pub type A = B;"), "{text}");
        assert!(v2.contains("pub newtype B : C"), "{text}");
        assert!(
            v2.contains("modify {\n      module api {\n        pub type C = .\n      }"),
            "{text}"
        );

        let replayed = replay(&plan.recomputed).expect("acyclic draft replays");
        assert!(compare(&replayed.current, &acyclic_live).is_empty());
        for leaf in ["A", "B", "C"] {
            let item = replayed
                .live_frozen
                .iter()
                .find(|item| item.entry.name == QualifiedName::new("api", leaf))
                .expect("live item");
            assert!(
                item.recursive_context.is_none(),
                "{leaf} must no longer retain the old recursive epoch"
            );
        }
    }

    #[test]
    fn label_reuse_adds_only_the_existing_contract_kinds() {
        let module_src = "\
module api;

pub labels { field: . };
pub labels Row = { field: _, other: . };
";
        let pkg_src = "package app;\n\nbridge {\n  api;\n}\n";
        let package = typed_package("api", module_src, pkg_src);
        let live = ContractSnapshot::from_package(&package);

        assert_eq!(
            live.items
                .keys()
                .map(|name| name.leaf.as_str())
                .collect::<Vec<_>>(),
            vec!["Field", "Other", "Row"]
        );
        assert!(matches!(
            live.items[&QualifiedName::new("api", "Field")].kind,
            ContractKind::Newtype { .. }
        ));
        assert!(matches!(
            live.items[&QualifiedName::new("api", "Row")].kind,
            ContractKind::Alias { .. }
        ));
    }

    #[test]
    fn live_snapshot_projects_newtype_payload_only_through_public_members() {
        let package = typed_package(
            "api",
            "\
module api;

type Hidden = .;
pub type Visible = .;

pub newtype Opaque : Hidden { constructor mk_opaque; projector un_opaque; };
pub newtype Exposed : Visible { pub constructor mk_exposed; projector un_exposed; };
",
            "package app; bridge { api; }",
        );
        let snapshot = ContractSnapshot::from_package(&package);

        let ContractKind::Newtype { surface, .. } =
            &snapshot.items[&QualifiedName::new("api", "Opaque")].kind
        else {
            panic!("Opaque should be a newtype");
        };
        assert_eq!(surface, &PublicNewtypeSurface::Opaque);

        let ContractKind::Newtype { surface, .. } =
            &snapshot.items[&QualifiedName::new("api", "Exposed")].kind
        else {
            panic!("Exposed should be a newtype");
        };
        assert_eq!(
            surface,
            &PublicNewtypeSurface::Constructor {
                name: "mk_exposed".to_owned(),
                payload: "@api/Visible".to_owned(),
            }
        );
    }

    #[test]
    fn hidden_newtype_details_do_not_change_the_public_contract() {
        let snapshot = |hidden: &str, constructor: &str, projector: &str| {
            let source = format!(
                "module api; \
                 type {hidden} = .; \
                 pub newtype Opaque : {hidden} {{ \
                   constructor {constructor}; \
                   projector {projector}; \
                 }};"
            );
            ContractSnapshot::from_package(&typed_package(
                "api",
                &source,
                "package app; bridge { api; }",
            ))
        };

        let left = snapshot("Hidden_left", "make_left", "take_left");
        let right = snapshot("Hidden_right", "make_right", "take_right");
        assert_eq!(left, right);
        assert_eq!(contract_digest(&left), contract_digest(&right));
        assert!(compare(&left, &right).is_empty());
    }

    #[test]
    fn recorded_opaque_newtype_omits_hidden_source_details() {
        let emitted =
            |hidden: &str, existential: &str, constructor: &str, projector: &str, scoped: bool| {
                let member_visibility = if scoped { "pub(api) " } else { "" };
                let source = format!(
                    "module api; \
                 type {hidden} = .; \
                 pub newtype Opaque <{existential}> : ({hidden} & {existential}) {{ \
                   {member_visibility}constructor {constructor}; \
                   {member_visibility}projector {projector}; \
                 }};"
                );
                let package = typed_package("api", &source, "package app; bridge { api; }");
                let live = ContractSnapshot::from_package(&package);
                let recorded = RecordedSurface::from_package(&package);
                let plan = compute_draft("app", None, &recorded, &live).expect("compute draft");
                let text = emit_signature_file(&plan.recomputed);
                let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
                    .expect("emitted sig must parse");
                let replayed = replay(&parsed).expect("emitted sig must replay");
                assert!(compare(&replayed.current, &live).is_empty(), "{text}");
                text
            };

        let left = emitted(
            "Hidden_left",
            "Existential_left",
            "make_left",
            "take_left",
            false,
        );
        let right = emitted(
            "Hidden_right",
            "Existential_right",
            "make_right",
            "take_right",
            true,
        );
        assert_eq!(left, right);
        for hidden in [
            "Hidden_left",
            "Existential_left",
            "make_left",
            "take_left",
            "Hidden_right",
            "Existential_right",
            "make_right",
            "take_right",
        ] {
            assert!(
                !left.contains(hidden),
                "hidden detail `{hidden}` leaked:\n{left}"
            );
            assert!(
                !right.contains(hidden),
                "hidden detail `{hidden}` leaked:\n{right}"
            );
        }
    }

    #[test]
    fn recorded_newtype_preserves_exact_public_member_role_and_payload() {
        let package = typed_package(
            "api",
            "module api; \
             pub type Visible = .; \
             pub newtype Ctor_only : Visible { \
               pub constructor sig_private_projector; \
               projector hidden_projector; \
             }; \
             pub newtype Proj_only : Visible { \
               constructor hidden_constructor; \
               pub projector take_visible; \
             };",
            "package app; bridge { api; }",
        );
        let live = ContractSnapshot::from_package(&package);
        assert_eq!(
            live.items[&QualifiedName::new("api", "Ctor_only")].kind,
            ContractKind::Newtype {
                param_kinds: vec![],
                surface: PublicNewtypeSurface::Constructor {
                    name: "sig_private_projector".to_owned(),
                    payload: "@api/Visible".to_owned(),
                },
            }
        );
        assert_eq!(
            live.items[&QualifiedName::new("api", "Proj_only")].kind,
            ContractKind::Newtype {
                param_kinds: vec![],
                surface: PublicNewtypeSurface::Projector {
                    name: "take_visible".to_owned(),
                    payload: "@api/Visible".to_owned(),
                },
            }
        );

        let recorded = RecordedSurface::from_package(&package);
        let plan = compute_draft("app", None, &recorded, &live).expect("compute draft");
        let text = emit_signature_file(&plan.recomputed);
        assert!(
            text.contains("pub constructor sig_private_projector;"),
            "{text}"
        );
        assert!(text.contains("pub projector take_visible };"), "{text}");
        assert!(!text.contains("hidden_projector"), "{text}");
        assert!(!text.contains("hidden_constructor"), "{text}");

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("emitted sig must parse");
        let replayed = replay(&parsed).expect("emitted sig must replay");
        assert_eq!(replayed.current, live);
    }

    #[test]
    fn parsed_signature_emission_canonicalizes_hidden_newtype_details() {
        let signature = |provider: &str, hidden: &str, constructor: &str, projector: &str| {
            format!(
                "signature app v(1); \
                 v(1) {{ nonbreaking {{ add {{ module api {{ \
                   import {provider}({hidden}); \
                   pub newtype Opaque : {hidden} {{ \
                     constructor {constructor}; \
                     projector {projector}; \
                   }}; \
                 }} }} }} }}"
            )
        };
        let left = crate::pass::parser::parse_signature_file(
            &signature("secret_left", "Hidden_left", "make_left", "take_left"),
            Some("app"),
        )
        .expect("left signature parses");
        let right = crate::pass::parser::parse_signature_file(
            &signature("secret_right", "Hidden_right", "make_right", "take_right"),
            Some("app"),
        )
        .expect("right signature parses");

        let left_text = emit_signature_file(&left);
        let right_text = emit_signature_file(&right);
        assert_eq!(left_text, right_text);
        for hidden in [
            "Hidden_left",
            "secret_left",
            "make_left",
            "take_left",
            "Hidden_right",
            "secret_right",
            "make_right",
            "take_right",
        ] {
            assert!(!left_text.contains(hidden), "{hidden} leaked:\n{left_text}");
            assert!(
                !right_text.contains(hidden),
                "{hidden} leaked:\n{right_text}"
            );
        }

        let replayed = replay(&left).expect("left signature replays");
        assert_eq!(
            replayed.current.items[&QualifiedName::new("api", "Opaque")].kind,
            ContractKind::Newtype {
                param_kinds: vec![],
                surface: PublicNewtypeSurface::Opaque,
            }
        );
    }

    #[test]
    fn hidden_newtype_trivia_does_not_change_the_signature_contract() {
        let signature = |side: &str| {
            format!(
                "signature app v(1);\n\
                 \n\
                 v(1) {{\n\
                   nonbreaking {{\n\
                     add {{\n\
                       module api {{\n\
                         pub newtype Opaque : . {{\n\
                           // {side} opaque constructor\n\
                           constructor make_opaque;\n\
                           // {side} opaque projector\n\
                           projector take_opaque;\n\
                           // {side} opaque dangling\n\
                         }};\n\
                         pub newtype Constructor_visible : . {{\n\
                           // fixed visible constructor\n\
                           pub constructor make_visible;\n\
                           // {side} hidden projector\n\
                           projector take_hidden;\n\
                           // fixed constructor-visible dangling\n\
                         }};\n\
                         pub newtype Projector_visible : . {{\n\
                           // {side} hidden constructor\n\
                           constructor make_hidden;\n\
                           // fixed visible projector\n\
                           pub projector take_visible;\n\
                           // fixed projector-visible dangling\n\
                         }};\n\
                       }}\n\
                     }}\n\
                   }}\n\
                 }}\n"
            )
        };
        let left = crate::pass::parser::parse_signature_file(&signature("left"), Some("app"))
            .expect("left signature parses");
        let right = crate::pass::parser::parse_signature_file(&signature("right"), Some("app"))
            .expect("right signature parses");

        let left_text = emit_signature_file(&left);
        let right_text = emit_signature_file(&right);
        assert_eq!(left_text, right_text);
        assert!(
            !left_text.contains("left"),
            "hidden trivia leaked:\n{left_text}"
        );
        assert!(
            !right_text.contains("right"),
            "hidden trivia leaked:\n{right_text}"
        );
        for retained in [
            "fixed visible constructor",
            "fixed constructor-visible dangling",
            "fixed visible projector",
            "fixed projector-visible dangling",
        ] {
            assert!(
                left_text.contains(retained),
                "public trivia {retained} was dropped:\n{left_text}"
            );
        }

        let left_snapshot = replay(&left).expect("left signature replays").current;
        let right_snapshot = replay(&right).expect("right signature replays").current;
        assert_eq!(left_snapshot, right_snapshot);
        assert!(compare(&left_snapshot, &right_snapshot).is_empty());
        assert_eq!(
            contract_digest(&left_snapshot),
            contract_digest(&right_snapshot)
        );
    }

    #[test]
    fn live_export_added_since_sig_is_compatible_drift() {
        let module_src = "\
module api;

host type Handle role(i32);

pub fn serve(h: Handle) -> Handle { h }

pub fn close(h: Handle) -> Handle { h }
";
        let pkg_src = "package app;\n\nbridge {\n  api;\n}\n";
        let package = typed_package("api", module_src, pkg_src);
        let live = ContractSnapshot::from_package(&package);

        // The sig knows only `Handle` + `serve`; `close` was added since.
        let sig_src = "\
signature app v(1);

v(1) {
  nonbreaking {
    add {
      module api {
        host type Handle role(i32);
        pub fn serve(h: Handle) -> Handle;
      }
    }

  }
}
";
        let file =
            crate::pass::parser::parse_signature_file(sig_src, Some("app")).expect("parse sig");
        let replayed = replay(&file).expect("replay");
        let report = compare(&replayed.current, &live);
        // Exactly one change: `close` added — a compatible export
        // addition.
        assert_eq!(report.changes.len(), 1, "{report:?}");
        assert_eq!(report.changes[0].name, QualifiedName::new("api", "close"));
        assert_eq!(report.changes[0].verdict, Verdict::Compatible);
        assert!(!report.is_breaking());
    }

    #[test]
    fn live_and_replayed_decl_binders_are_alpha_equivalent() {
        let module_src = "\
module api;

pub type Identity[A] = A;

pub newtype Pack[A] <Hidden> : (A & Hidden) {
  pub constructor pack;
  pub projector unpack;
};
";
        let pkg_src = "package app;\n\nbridge {\n  api;\n}\n";
        let package = typed_package("api", module_src, pkg_src);
        let live = ContractSnapshot::from_package(&package);

        let sig_src = "\
signature app v(1);

v(1) {
  nonbreaking {
    add {
      module api {
        pub type Identity[Value] = Value;
        pub newtype Pack[Visible] <Sealed> : (Visible & Sealed) {
          pub constructor pack;
          pub projector unpack;
        };
      }
    }

  }
}
";
        let file =
            crate::pass::parser::parse_signature_file(sig_src, Some("app")).expect("parse sig");
        let replayed = replay(&file).expect("replay");
        let report = compare(&replayed.current, &live);
        assert!(
            report.is_empty(),
            "declaration binders are alpha-insensitive and existential prefixes agree: \
             {report:?}",
        );
    }

    #[test]
    fn replayed_generic_aliases_keep_selective_and_qualified_nominal_identity() {
        let sig_src = "\
signature app v(1);

v(1) {
  nonbreaking {
    add {
      module left {
        pub newtype Same[A] : A { constructor make; projector take; };
      };
      module right {
        pub newtype Same[A] : A { constructor make; projector take; };
      };
      module api {
        import left(Same);
        import right as r;
        pub type Selected[Value] = Same(Value);
        pub type Qualified[Item] = r.Same(Item);
      }
    }
  }
}
";
        let file =
            crate::pass::parser::parse_signature_file(sig_src, Some("app")).expect("parse sig");
        let replayed = replay(&file).expect("replay");
        let expansion = |name: &str| {
            let ContractKind::Alias { expansion, .. } =
                &replayed.current.items[&QualifiedName::new("api", name)].kind
            else {
                panic!("{name} should be an alias");
            };
            expansion.as_str()
        };
        assert_eq!(expansion("Selected"), "@left/Same(#tv0)");
        assert_eq!(expansion("Qualified"), "@right/Same(#tv0)");
    }

    #[test]
    fn modify_referencing_earlier_same_module_type_qualifies_module_local() {
        // v(1) adds `host type H` + `pub fn f(x: H) -> H`; v(2) modifies
        // `f` to `pub fn f() -> H` WITHOUT redeclaring `H` in the v(2)
        // section. The section qualifier must still resolve the bare `H`
        // head module-locally (to `@api/H`) — the same-module-type oracle
        // is the whole changelog's declared types, not just the v(2)
        // section's. A section-local-only oracle would canonicalize `H`
        // to `@H`, diverging from the live snapshot and producing a
        // spurious break.
        let module_src = "\
module api;

host type H role(i32);

pub fn f() -> H { todo_h() }

host fn todo_h() -> H;
";
        let pkg_src = "package app;\n\nbridge {\n  api;\n}\n";
        let package = typed_package("api", module_src, pkg_src);
        let live = ContractSnapshot::from_package(&package);

        let sig_src = "\
signature app v(2);

v(1) {
  nonbreaking {
    add {
      module api {
        host type H role(i32);
        host fn todo_h() -> H;
        pub fn f(x: H) -> H;
      }
    }
  }
}

v(2) {
  breaking {
    modify {
      module api {
        pub fn f() -> H;
      }
    }
  }
}
";
        let file =
            crate::pass::parser::parse_signature_file(sig_src, Some("app")).expect("parse sig");
        let replayed = replay(&file).expect("replay");

        // The replayed `f`'s return type must canonicalize to `@api/H`,
        // not the unqualified `@H`.
        let ContractKind::Fn { signature, .. } =
            &replayed.current.items[&QualifiedName::new("api", "f")].kind
        else {
            panic!("f should be a fn");
        };
        assert!(
            signature.contains("@api/H"),
            "modified f's return type must qualify module-locally to @api/H: {signature}"
        );
        assert!(
            !signature.contains("(@H)") && !signature.contains(">@H") && !signature.ends_with("@H"),
            "modified f must not carry an unqualified @H head: {signature}"
        );

        // And the replayed current interface compares clean against the
        // live surface — no spurious break from the unredeclared `H`.
        let report = compare(&replayed.current, &live);
        assert!(
            report.is_empty(),
            "modify referencing an earlier same-module type must not spuriously break: {report:?}",
        );
    }

    /// The recorded-surface declarations, emitted to `.sig.kio` text,
    /// re-parsed, and replayed, compare clean against the live snapshot.
    /// This is the full write-path round trip: record → emit → parse →
    /// replay → compare must be empty (the recorded sig describes
    /// exactly the live surface).
    fn assert_round_trips(module_path: &str, module_src: &str, pkg_src: &str) {
        let package = typed_package(module_path, module_src, pkg_src);
        let live = ContractSnapshot::from_package(&package);
        let recorded = RecordedSurface::from_package(&package);

        // Compute the fresh-package draft (no prior sig) and emit it.
        let plan = compute_draft("app", None, &recorded, &live).expect("compute draft");
        let text = emit_signature_file(&plan.recomputed);

        // The emitted text must parse, replay, and compare clean.
        let parsed =
            crate::pass::parser::parse_signature_file(&text, Some("app")).unwrap_or_else(|e| {
                panic!("emitted sig must re-parse; text was:\n{text}\nerror: {e:?}")
            });
        let replayed = replay(&parsed).expect("replay emitted sig");
        let report = compare(&replayed.current, &live);
        assert!(
            report.is_empty(),
            "emitted sig must describe the live surface exactly; report: {report:?}\ntext:\n{text}"
        );
        // A fresh package records every contract item as an addition
        // against the empty sealed baseline; every placement is `Added`
        // (host requirements are breaking adds, exports compatible).
        assert!(!plan.report.is_empty(), "fresh package records its surface");
        assert!(
            plan.report
                .changes
                .iter()
                .all(|c| c.placement == ChangePlacement::Added),
            "fresh-package draft is all additions: {:?}",
            plan.report
        );
    }

    #[test]
    fn record_emit_replay_round_trips_single_module() {
        assert_round_trips(
            "api",
            "\
module api;

host type Handle role(i32);

host fn open() -> Handle;

newtype Box : Handle { pub constructor mk_box; pub projector un_box; };

type Alias = Handle;

pub fn serve[A](h: Handle, x: A) -> Handle { h }
",
            "package app;\n\nbridge {\n  api;\n}\n",
        );
    }

    #[test]
    fn record_emit_replay_preserves_marked_type_names() {
        let package = typed_package(
            "api",
            "\
module api;

host type _String role(str);

pub type _Alias[_A] = _A;

pub newtype _Box[_A] : _A { pub constructor mk_box; pub projector un_box; };
",
            "package app;\n\nbridge {\n  api;\n}\n",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = RecordedSurface::from_package(&package);
        let plan = compute_draft("app", None, &recorded, &live).expect("compute draft");
        let text = emit_signature_file(&plan.recomputed);

        for spelling in ["_String", "_Alias[_A]", "_Box[_A]"] {
            assert!(text.contains(spelling), "missing `{spelling}`:\n{text}");
        }

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("marked-name signature must re-parse");
        let replayed = replay(&parsed).expect("marked-name signature must replay");
        let report = compare(&replayed.current, &live);
        assert!(
            report.is_empty(),
            "marked names changed across record/emit/parse/replay: {report:?}\n{text}"
        );
    }

    #[test]
    fn record_emit_replay_preserves_exported_function_purity() {
        let package = lowered_package(
            "api",
            "module api; pub pure fn id[A](x: A) -> A { x }",
            "package app; bridge { api; }",
        );
        let live = ContractSnapshot::from_package(&package);
        let recorded = RecordedSurface::from_package(&package);
        let plan = compute_draft("app", None, &recorded, &live).expect("compute draft");
        let text = emit_signature_file(&plan.recomputed);
        assert!(
            text.contains("pub pure fn id[A](x: A) -> A\n      }"),
            "{text}"
        );

        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .expect("emitted sig must parse");
        let replayed = replay(&parsed).expect("emitted sig must replay");
        assert!(compare(&replayed.current, &live).is_empty());
    }

    #[test]
    fn record_emit_replay_round_trips_cross_module() {
        // Two modules; `main` references `core`'s exported newtype, so
        // the recorded `main` section must carry the `import` that resolves
        // it. The package's bridge selects both.
        let module_src = "\
module core;

host type Raw role(i32);

pub newtype Id : Raw { pub constructor mk_id; pub projector un_id; };
";
        // Single-module helper only lowers one module, so build the
        // two-module package directly here.
        use crate::ast::{Meta, PackageFile, Prime, Surface};
        use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry, TopLevelScope};
        use std::path::PathBuf;

        let desugar = |src: &str| -> crate::ast::Module<crate::ast::Desugared> {
            let module: crate::ast::Module<Surface> =
                crate::pass::parser::parse(src).expect("parse module");
            crate::pass::desugar::desugar_module(module).expect("desugar")
        };
        let core = desugar(module_src);
        let main = desugar(
            "\
module main;

import core(Id);

pub fn wrap(x: Id) -> Id { x }
",
        );
        let (lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![
                (PathBuf::from("core.kio"), core),
                (PathBuf::from("main.kio"), main),
            ],
            None,
        )
        .expect("elaborate");
        let elabs = crate::pass::typecheck_full::Elaborations::new();
        let mut modules = std::collections::BTreeMap::new();
        for (path, lowered_module) in lowered {
            let prime = crate::pass::substitute::substitute_module(&lowered_module, &elabs);
            let scope = TopLevelScope::build(&prime).expect("scope");
            let key = prime
                .path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("m.kio");
            modules.insert(
                key,
                ModuleEntry::<Prime> {
                    file_path: PathBuf::from(file_name),
                    module: prime,
                    scope,
                },
            );
        }
        let pkg_src = "package app;\n\nbridge {\n  core;\n  main;\n}\n";
        let surface_pkg =
            crate::pass::parser::parse_package_file(pkg_src, Some("app")).expect("parse pkg");
        let pkg_prime = PackageFile::<Prime> {
            name: surface_pkg.name,
            build: surface_pkg.build,
            bridge: surface_pkg.bridge,
            meta: Meta::new(Span::new(0, 0)),
        };
        let package = Package::<Prime>::from_parts(
            modules,
            Some(PackageFileEntry {
                file_path: PathBuf::from("app.pkg.kio"),
                package_name: "app".to_owned(),
                package_file: pkg_prime,
            }),
        );

        let live = ContractSnapshot::from_package(&package);
        let recorded = RecordedSurface::from_package(&package);
        let plan = compute_draft("app", None, &recorded, &live).expect("compute draft");
        let text = emit_signature_file(&plan.recomputed);
        let parsed = crate::pass::parser::parse_signature_file(&text, Some("app"))
            .unwrap_or_else(|e| panic!("emitted sig must re-parse:\n{text}\nerror: {e:?}"));
        let replayed = replay(&parsed).expect("replay");
        let report = compare(&replayed.current, &live);
        assert!(
            report.is_empty(),
            "cross-module sig must describe the live surface exactly: {report:?}\n{text}"
        );
    }
}

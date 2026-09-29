//! Swift backend — the typed FFI skin.
//!
//! Swift's body is erased (`Any` / `[Any]`), but the host's contract is
//! **typed**: a product crosses the boundary as a generic Swift `struct`, a
//! sum as a generic native `enum` with associated values, an exact nullary
//! host type as its host-selected associated type, and every other public
//! nominal or application through an exact generic carrier. The prepared
//! boundary transaction owns that public skin. Expression literals retain a
//! separate, narrow [`SwiftLiteralContext`] containing only the routed package
//! and the exact nullary host-type table needed by typed literal emission.
//!
//! ## Body representation (shared with the body emit)
//!
//! The body's `[Any]` rep is the JS dynamic body's shape, in Swift:
//!
//! - **product** `(A & B & C)` → right-nested `[a, [b, c]]`; slot `i` is
//!   reached by `i` nested `[1]` peels then `[0]` (unless the last slot).
//! - **sum** `(A | B | C)` → tagged `[tag, payload]` with tag `0` = left
//!   arm, `1` = right-remainder; arm `k` is reached by peeling `[1]` `k`
//!   times.
//! - **unit** → `Unit()`; **atomic / function / type-var** flow through
//!   unchanged.
//!
//! The skin's job is the typed↔erased bridge at the boundary; the body
//! itself never re-keys (an erased `[Any]` carries everything).
//!
//! ## Native sums
//!
//! Swift uses a native `enum` with associated values, matched by an
//! **exhaustive `switch`** (no default arm — the enum is closed, so the host
//! cannot supply a foreign case). This is the native-sum end of the
//! erased-static family.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use crate::ast::{Role, Routed, Type};
use crate::backends::boundary_facade::{
    BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryFacadeSupportOrigin,
    BoundaryHostBindingOrigin, BoundaryHostTypeBinding, BoundaryNewtypeSurface, FacadeKind,
    FacadeShellId, FacadeUse, FacadeUseId, PreparedBoundaryCallableSites, QualifiedTypeName,
    SemanticKey,
};
use crate::backends::public_names::encode_host_identity;
use crate::backends::skin::ExactHostTypeTable;

/// One exact host declaration in Swift's prepared package boundary.
#[derive(Clone, Debug)]
pub(crate) struct PreparedSwiftHostBinding {
    pub(crate) name: QualifiedTypeName,
    pub(crate) type_arity: usize,
    pub(crate) role: Option<Role>,
    pub(crate) origin: BoundaryHostBindingOrigin,
}

/// Swift's package-complete public-shape realization of one prepared facade
/// transaction. The erased body still uses `Any`, but every public nominal,
/// product, sum, and application has a declaration-owned exact carrier.
pub(crate) struct PreparedSwiftShapes {
    host_protocol: String,
    host_bindings: BTreeMap<QualifiedTypeName, PreparedSwiftHostBinding>,
    facade_shells: BTreeMap<FacadeShellId, BoundaryFacadeSupportOrigin>,
    application_arities: BTreeMap<usize, BoundaryFacadeSupportOrigin>,
    forall_carriers: Mutex<BTreeMap<String, String>>,
    #[cfg(all(test, feature = "surface"))]
    reached_nominal_index_construction_visits: usize,
    #[cfg(all(test, feature = "surface"))]
    reached_nominal_index_lookups: usize,
}

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SwiftReachedNominalIndexMeasurement {
    pub(crate) retained_binding_count: usize,
    pub(crate) selected_binding_count: usize,
    pub(crate) index_construction_visits: usize,
    pub(crate) indexed_lookups: usize,
    pub(crate) legacy_candidate_visits: usize,
    pub(crate) identity_parity_checks: usize,
}

#[cfg(not(all(test, feature = "surface")))]
fn reached_nominal_index_contains(
    reached_nominals: &BTreeSet<QualifiedTypeName>,
    name: &QualifiedTypeName,
) -> bool {
    reached_nominals.contains(name)
}

#[cfg(all(test, feature = "surface"))]
fn reached_nominal_index_contains(
    reached_nominals: &BTreeSet<QualifiedTypeName>,
    name: &QualifiedTypeName,
    indexed_lookups: &std::cell::Cell<usize>,
) -> bool {
    indexed_lookups.set(indexed_lookups.get() + 1);
    reached_nominals.contains(name)
}

impl PreparedSwiftShapes {
    pub(crate) fn new(prepared: &PreparedBoundaryCallableSites, host_protocol: &str) -> Self {
        // Collapse Swift-owned support provenance once. Rendering then walks
        // canonical maps directly instead of rescanning every retained site
        // for each declaration class.
        #[cfg(all(test, feature = "surface"))]
        let reached_nominal_index_construction_visits = std::cell::Cell::new(0);
        let reached_nominals = prepared
            .sites()
            .flat_map(|site| {
                #[cfg(all(test, feature = "surface"))]
                let construction_visits = &reached_nominal_index_construction_visits;
                site.nominals().declarations().map(move |(name, _)| {
                    #[cfg(all(test, feature = "surface"))]
                    construction_visits.set(construction_visits.get() + 1);
                    name.clone()
                })
            })
            .collect::<BTreeSet<_>>();
        #[cfg(all(test, feature = "surface"))]
        let reached_nominal_index_lookups = std::cell::Cell::new(0);
        let host_bindings: BTreeMap<QualifiedTypeName, PreparedSwiftHostBinding> = prepared
            .host_bindings()
            .filter(|binding| {
                if matches!(binding.origin(), BoundaryHostBindingOrigin::Live) {
                    true
                } else {
                    #[cfg(all(test, feature = "surface"))]
                    {
                        reached_nominal_index_contains(
                            &reached_nominals,
                            binding.name(),
                            &reached_nominal_index_lookups,
                        )
                    }
                    #[cfg(not(all(test, feature = "surface")))]
                    {
                        reached_nominal_index_contains(&reached_nominals, binding.name())
                    }
                }
            })
            .map(|binding| {
                let name = binding.name().clone();
                let role = match binding.binding() {
                    BoundaryHostTypeBinding::Role(role) => Some(role),
                    BoundaryHostTypeBinding::Roleless => None,
                };
                (
                    name.clone(),
                    PreparedSwiftHostBinding {
                        name,
                        type_arity: binding.type_params().len(),
                        role,
                        origin: binding.origin(),
                    },
                )
            })
            .collect();
        let facade_shells = prepared
            .shell_origins()
            .map(|(shell, origin)| (shell.clone(), origin))
            .collect();
        let mut application_arities = BTreeMap::new();
        for site in prepared.sites() {
            let origin = site
                .retained()
                .map_or(BoundaryFacadeSupportOrigin::Live, |metadata| {
                    BoundaryFacadeSupportOrigin::Retained {
                        removed_at_version: metadata.removed_at_version(),
                    }
                });
            collect_application_origins(site.plan().facade(), origin, &mut application_arities);
            for (_, declaration) in site.nominals().declarations() {
                if let crate::backends::boundary_facade::BoundaryNominalDeclaration::Newtype {
                    transparent_payload: Some(payload),
                    ..
                } = declaration
                {
                    collect_application_origins(payload.facade(), origin, &mut application_arities);
                }
            }
        }
        for binding in host_bindings
            .values()
            .filter(|binding| binding.type_arity > 0)
        {
            let origin = match binding.origin {
                BoundaryHostBindingOrigin::Live => BoundaryFacadeSupportOrigin::Live,
                BoundaryHostBindingOrigin::Retained { removed_at_version } => {
                    BoundaryFacadeSupportOrigin::Retained { removed_at_version }
                }
            };
            merge_support_origin(&mut application_arities, binding.type_arity, origin);
        }
        for entry in prepared
            .public_newtypes()
            .filter(|entry| !entry.type_params().is_empty())
        {
            merge_support_origin(
                &mut application_arities,
                entry.type_params().len(),
                BoundaryFacadeSupportOrigin::Live,
            );
        }
        for entry in prepared
            .retained_public_newtypes()
            .filter(|entry| !entry.type_params().is_empty())
        {
            let removed_at_version = prepared
                .retained_public_newtype_removed_at(entry.name())
                .expect("a retained Swift newtype carrier has removal provenance");
            merge_support_origin(
                &mut application_arities,
                entry.type_params().len(),
                BoundaryFacadeSupportOrigin::Retained { removed_at_version },
            );
        }
        for origin in application_arities.values().copied().collect::<Vec<_>>() {
            merge_support_origin(&mut application_arities, 1, origin);
        }
        Self {
            host_protocol: host_protocol.to_owned(),
            host_bindings,
            facade_shells,
            application_arities,
            forall_carriers: Mutex::new(BTreeMap::new()),
            #[cfg(all(test, feature = "surface"))]
            reached_nominal_index_construction_visits: reached_nominal_index_construction_visits
                .get(),
            #[cfg(all(test, feature = "surface"))]
            reached_nominal_index_lookups: reached_nominal_index_lookups.get(),
        }
    }

    pub(crate) fn host_protocol(&self) -> &str {
        &self.host_protocol
    }

    pub(crate) fn live_host_bindings(&self) -> impl Iterator<Item = &PreparedSwiftHostBinding> {
        self.host_bindings
            .values()
            .filter(|binding| matches!(binding.origin, BoundaryHostBindingOrigin::Live))
    }

    pub(crate) fn retained_host_bindings(&self) -> impl Iterator<Item = &PreparedSwiftHostBinding> {
        self.host_bindings
            .values()
            .filter(|binding| matches!(binding.origin, BoundaryHostBindingOrigin::Retained { .. }))
    }

    pub(crate) fn host_binding(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&PreparedSwiftHostBinding> {
        self.host_bindings.get(name)
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn measure_reached_nominal_index(
        &self,
        prepared: &PreparedBoundaryCallableSites,
    ) -> SwiftReachedNominalIndexMeasurement {
        let mut reached_nominals = BTreeSet::new();
        let mut parity_construction_visits = 0;
        for site in prepared.sites() {
            for (name, _) in site.nominals().declarations() {
                parity_construction_visits += 1;
                reached_nominals.insert(name.clone());
            }
        }
        assert_eq!(
            self.reached_nominal_index_construction_visits,
            parity_construction_visits
        );

        let mut retained_binding_count = 0;
        let mut legacy_candidate_visits = 0;
        let mut identity_parity_checks = 0;
        let mut indexed_selected = BTreeSet::new();
        let mut legacy_selected = BTreeSet::new();
        for binding in prepared.host_bindings() {
            let live = matches!(binding.origin(), BoundaryHostBindingOrigin::Live);
            if !live {
                retained_binding_count += 1;
            }
            let indexed_reached = live || reached_nominals.contains(binding.name());
            let legacy_reached = live
                || prepared.sites().any(|site| {
                    site.nominals().declarations().any(|(candidate, _)| {
                        legacy_candidate_visits += 1;
                        candidate == binding.name()
                    })
                });
            assert_eq!(
                indexed_reached, legacy_reached,
                "the Swift reached-nominal index changed exact binding selection"
            );
            if indexed_reached {
                indexed_selected.insert(binding.name().clone());
            }
            if legacy_reached {
                legacy_selected.insert(binding.name().clone());
            }
            identity_parity_checks += 1;
        }

        let actual_selected = self.host_bindings.keys().cloned().collect::<BTreeSet<_>>();
        assert_eq!(actual_selected, indexed_selected);
        assert_eq!(actual_selected, legacy_selected);

        SwiftReachedNominalIndexMeasurement {
            retained_binding_count,
            selected_binding_count: actual_selected.len(),
            index_construction_visits: self.reached_nominal_index_construction_visits,
            indexed_lookups: self.reached_nominal_index_lookups,
            legacy_candidate_visits,
            identity_parity_checks,
        }
    }

    pub(crate) fn register_forall(&self, name: String, declaration: String) {
        let previous = self
            .forall_carriers
            .lock()
            .expect("Swift forall-carrier registry mutex is not poisoned")
            .insert(name.clone(), declaration.clone());
        if let Some(previous) = previous {
            assert_eq!(
                previous, declaration,
                "one exact Swift forall carrier received two declarations: {name}"
            );
        }
    }

    pub(crate) fn render_shapes(&self, prepared: &PreparedBoundaryCallableSites) -> String {
        let mut out = String::from("// Generated by kio — do not edit by hand.\n");
        if let Some(origin) = self.application_arities.get(&1) {
            let deprecation = origin.removed_at_version().map(|version| {
                format!("Kio native application support is retained only for removed host declarations from contract v{version}")
            });
            let deprecated = swift_optional_deprecated_attribute(deprecation.as_deref(), "");
            let member = swift_optional_deprecated_attribute(deprecation.as_deref(), "\t");
            out.push_str(&format!(
                "\n{deprecated}public enum KioNative<Root> {{\n\
                 {member}\tpublic static var constructor: KioNativeConstructor<KioNative<Root>> {{ .init() }}\n\
                 }}\n\n\
                 {deprecated}public struct KioNativeConstructor<F> {{\n\
                 \tfileprivate init() {{}}\n\
                 {member}\tpublic func applying<A>(_: A.Type) -> KioNativeConstructor<KioApply1<F, A>> {{ .init() }}\n\
                 {member}\tpublic func lift<A, Storage>(_ value: Storage) -> KioApply1<F, A> {{ .init(value) }}\n\
                 {member}\tpublic func project<A, Storage>(_ value: KioApply1<F, A>, as _: Storage.Type = Storage.self) -> Storage {{ value.__kioValue as! Storage }}\n\
                 }}\n"
            ));
        }
        for (shell, origin) in &self.facade_shells {
            render_prepared_shell(shell, *origin, &mut out);
        }
        for binding in self
            .host_bindings
            .values()
            .filter(|binding| binding.type_arity > 0)
        {
            render_parameterized_host_carrier(binding, &mut out);
        }
        for entry in prepared.public_newtypes() {
            render_prepared_newtype_carrier(entry, &self.host_protocol, None, &mut out);
        }
        for entry in prepared.retained_public_newtypes() {
            render_prepared_newtype_carrier(
                entry,
                &self.host_protocol,
                prepared.retained_public_newtype_removed_at(entry.name()),
                &mut out,
            );
        }
        for declaration in self
            .forall_carriers
            .lock()
            .expect("Swift forall-carrier registry mutex is not poisoned")
            .values()
        {
            out.push_str(declaration);
        }
        for (arity, origin) in &self.application_arities {
            render_application_carrier(*arity, *origin, &mut out);
        }
        out
    }
}

pub(crate) fn swift_facade_shell_name(shell: &FacadeShellId) -> String {
    shell.encode_public()
}

pub(crate) fn swift_semantic_member(key: &SemanticKey) -> String {
    match key {
        SemanticKey::Bare { name } => {
            swift_field_name(&crate::backends::public_names::host_name_core(name))
        }
        SemanticKey::Qualified {
            module_segments,
            name,
        } => format!(
            "KioQualified_{}__{}",
            encode_host_identity(&module_segments.join("/")),
            encode_host_identity(name)
        ),
        SemanticKey::Positional { index } => format!("_{index}"),
    }
}

pub(crate) fn swift_nominal_identity(name: &QualifiedTypeName) -> String {
    format!(
        "{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

pub(crate) fn swift_host_carrier_name(name: &QualifiedTypeName) -> String {
    format!("KioHostType_{}", swift_nominal_identity(name))
}

pub(crate) fn swift_host_constructor_name(name: &QualifiedTypeName) -> String {
    format!("KioHostTypeMk_{}", swift_nominal_identity(name))
}

pub(crate) fn swift_newtype_carrier_name(name: &QualifiedTypeName) -> String {
    format!("KioNewtype_{}", swift_nominal_identity(name))
}

pub(crate) fn swift_newtype_constructor_name(name: &QualifiedTypeName) -> String {
    format!("KioNewtypeMk_{}", swift_nominal_identity(name))
}

pub(crate) fn swift_apply_carrier_name(arity: usize) -> String {
    format!("KioApply{arity}")
}

pub(crate) fn swift_forall_carrier_name(
    site: &BoundaryFacadeSiteId,
    use_id: FacadeUseId,
) -> String {
    format!(
        "KioForall_{}__{}_U{}",
        encode_host_identity(&site.module_segments().join("/")),
        swift_forall_owner_name(site.owner()),
        use_id.index()
    )
}

fn swift_forall_owner_name(owner: &BoundaryFacadeSiteOwner) -> String {
    match owner {
        BoundaryFacadeSiteOwner::HostFunction { name } => {
            format!("HostFn_u{}", encode_host_identity(name))
        }
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            format!("ExportFn_u{}", encode_host_identity(name))
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
            format!(
                "NewtypeCtor_u{}__{}",
                encode_host_identity(newtype),
                encode_host_identity(member)
            )
        }
        BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
            format!(
                "NewtypeProj_u{}__{}",
                encode_host_identity(newtype),
                encode_host_identity(member)
            )
        }
    }
}

fn swift_generic_declaration(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", parameters.join(", "))
    }
}

pub(crate) fn swift_generic_use(parameters: &[String]) -> String {
    swift_generic_declaration(parameters)
}

fn render_prepared_shell(
    shell: &FacadeShellId,
    origin: BoundaryFacadeSupportOrigin,
    out: &mut String,
) {
    let name = swift_facade_shell_name(shell);
    let parameters = (0..shell.ordered_keys().len())
        .map(|index| format!("T{index}"))
        .collect::<Vec<_>>();
    let declaration = swift_generic_declaration(&parameters);
    let deprecation = origin.removed_at_version().map(|version| {
        format!(
            "Kio facade shell `{name}` is retained only for removed host declarations from contract v{version}"
        )
    });
    out.push('\n');
    match shell.kind() {
        FacadeKind::Product => {
            let fields = shell
                .ordered_keys()
                .iter()
                .enumerate()
                .map(|(index, key)| (swift_semantic_member(key), format!("T{index}")))
                .collect::<Vec<_>>();
            out.push_str(&super::emit::render_swift_struct(
                &format!("{name}{declaration}"),
                &fields,
                deprecation.as_deref(),
            ));
        }
        FacadeKind::Sum => {
            out.push_str(&swift_optional_deprecated_attribute(
                deprecation.as_deref(),
                "",
            ));
            out.push_str(&format!("public enum {name}{declaration} {{\n"));
            for (index, key) in shell.ordered_keys().iter().enumerate() {
                out.push_str(&swift_optional_deprecated_attribute(
                    deprecation.as_deref(),
                    "\t",
                ));
                out.push_str(&format!(
                    "\tcase {}(T{index})\n",
                    swift_semantic_member(key)
                ));
            }
            out.push_str("}\n");
        }
    }
}

fn render_parameterized_host_carrier(binding: &PreparedSwiftHostBinding, out: &mut String) {
    let name = swift_host_carrier_name(&binding.name);
    let marker = swift_host_constructor_name(&binding.name);
    let native_identity = format!(
        "KioHostTypeIdentity_{}",
        swift_nominal_identity(&binding.name)
    );
    let parameters = (0..binding.type_arity)
        .map(|index| format!("T{index}"))
        .collect::<Vec<_>>();
    let declaration = swift_generic_declaration(&parameters);
    let deprecation = match binding.origin {
        BoundaryHostBindingOrigin::Live => None,
        BoundaryHostBindingOrigin::Retained { removed_at_version } => Some(format!(
            "Kio host type `{}.{}` is retained only for removed host declarations from contract v{removed_at_version}",
            binding.name.module_segments().join("."),
            binding.name.name()
        )),
    };
    let deprecated = swift_optional_deprecated_attribute(deprecation.as_deref(), "");
    let member_deprecated = swift_optional_deprecated_attribute(deprecation.as_deref(), "\t");
    out.push_str(&format!(
        "\n{deprecated}public enum {native_identity} {{}}\n{deprecated}public typealias {marker} = KioNative<{native_identity}>\n\n{deprecated}public struct {name}{declaration} {{\n\tlet __kioValue: Any\n{member_deprecated}\tpublic init<Storage>(_ value: Storage) {{ self.__kioValue = value }}\n{member_deprecated}\tpublic func value<Storage>(as _: Storage.Type = Storage.self) -> Storage {{ self.__kioValue as! Storage }}\n}}\n"
    ));
}

fn render_prepared_newtype_carrier(
    entry: &crate::backends::boundary_facade::BoundaryPublicNewtypeInventoryEntry,
    host_protocol: &str,
    retained_at_version: Option<u32>,
    out: &mut String,
) {
    let name = swift_newtype_carrier_name(entry.name());
    let marker = swift_newtype_constructor_name(entry.name());
    let mut parameters = vec![format!("H: {host_protocol}")];
    parameters.extend((0..entry.type_params().len()).map(|index| format!("T{index}")));
    let declaration = swift_generic_declaration(&parameters);
    let deprecated = retained_at_version
        .map(|version| {
            swift_deprecated_attribute(&format!(
                "Kio newtype `{}.{}` is retained only for removed host declarations from contract v{version}",
                entry.name().module_segments().join("."),
                entry.name().name()
            ))
        })
        .unwrap_or_default();
    if !entry.type_params().is_empty() {
        out.push_str(&format!(
            "\n{deprecated}public enum {marker}<H: {host_protocol}> {{}}\n"
        ));
    }
    let surface = match entry.surface() {
        BoundaryNewtypeSurface::Unexposed => "unexposed",
        BoundaryNewtypeSurface::Opaque => "opaque",
        BoundaryNewtypeSurface::Constructor { .. } => "constructor",
        BoundaryNewtypeSurface::Projector { .. } => "projector",
        BoundaryNewtypeSurface::Both { .. } => "constructor and projector",
    };
    out.push_str(&format!(
        "\n// Exact {surface} carrier for Kio newtype `{}.{}`.\n{deprecated}public struct {name}{declaration} {{\n\tlet __kioValue: Any\n\tinit(_ value: Any) {{ self.__kioValue = value }}\n}}\n",
        entry.name().module_segments().join("."),
        entry.name().name()
    ));
}

pub(crate) fn swift_deprecated_attribute(message: &str) -> String {
    format!("@available(*, deprecated, message: \"{message}\")\n")
}

fn swift_optional_deprecated_attribute(message: Option<&str>, indent: &str) -> String {
    message
        .map(|message| format!("{indent}{}", swift_deprecated_attribute(message)))
        .unwrap_or_default()
}

fn merge_support_origin(
    origins: &mut BTreeMap<usize, BoundaryFacadeSupportOrigin>,
    key: usize,
    incoming: BoundaryFacadeSupportOrigin,
) {
    origins
        .entry(key)
        .and_modify(|existing| {
            *existing = match (*existing, incoming) {
                (BoundaryFacadeSupportOrigin::Live, _) | (_, BoundaryFacadeSupportOrigin::Live) => {
                    BoundaryFacadeSupportOrigin::Live
                }
                (
                    BoundaryFacadeSupportOrigin::Retained {
                        removed_at_version: left,
                    },
                    BoundaryFacadeSupportOrigin::Retained {
                        removed_at_version: right,
                    },
                ) => BoundaryFacadeSupportOrigin::Retained {
                    removed_at_version: left.min(right),
                },
            };
        })
        .or_insert(incoming);
}

fn collect_application_origins(
    plan: &crate::backends::boundary_facade::BoundaryFacadePlan,
    origin: BoundaryFacadeSupportOrigin,
    origins: &mut BTreeMap<usize, BoundaryFacadeSupportOrigin>,
) {
    for use_ in plan.uses() {
        if let FacadeUse::Apply { args, .. } = use_ {
            merge_support_origin(origins, args.len(), origin);
        }
    }
}

fn render_application_carrier(arity: usize, origin: BoundaryFacadeSupportOrigin, out: &mut String) {
    let name = swift_apply_carrier_name(arity);
    let mut parameters = vec!["F".to_owned()];
    parameters.extend((0..arity).map(|index| format!("T{index}")));
    let declaration = swift_generic_declaration(&parameters);
    let deprecation = origin.removed_at_version().map(|version| {
        format!(
            "Kio application carrier `{name}` is retained only for removed host declarations from contract v{version}"
        )
    });
    let deprecated = swift_optional_deprecated_attribute(deprecation.as_deref(), "");
    if arity == 1 {
        out.push_str(&format!(
            "\n{deprecated}public struct {name}{declaration} {{\n\tlet __kioValue: Any\n\tinit(_ value: Any) {{ self.__kioValue = value }}\n}}\n"
        ));
    } else {
        let mut applied = "F".to_owned();
        for index in 0..arity {
            applied = format!("KioApply1<{applied}, T{index}>");
        }
        out.push_str(&format!(
            "\n{deprecated}public typealias {name}{declaration} = {applied}\n"
        ));
    }
}

/// Minimal semantic context used by expression-literal emission. Public FFI
/// shapes come exclusively from [`PreparedSwiftShapes`]; literal lowering
/// needs only the routed package for local type reconstruction and the exact
/// nullary host declaration selected by an annotation or expected type.
pub(crate) struct SwiftLiteralContext<'p> {
    package: &'p crate::pass::resolve::Package<Routed>,
    exact_host_types: ExactHostTypeTable,
}

impl<'p> SwiftLiteralContext<'p> {
    pub(crate) fn new(
        package: &'p crate::pass::resolve::Package<Routed>,
        exact_host_types: ExactHostTypeTable,
    ) -> Self {
        Self {
            package,
            exact_host_types,
        }
    }

    pub(crate) fn package(&self) -> &'p crate::pass::resolve::Package<Routed> {
        self.package
    }

    pub(crate) fn exact_host_type_of(
        &self,
        ty: &Type<Routed>,
    ) -> Option<(String, String, Option<Role>)> {
        if !matches!(ty, Type::Path { args, .. } if args.is_empty()) {
            return None;
        }
        let (module, name) = crate::host_descriptor::routed_host_type_identity(ty)?;
        let role = *self
            .exact_host_types
            .get(&(module.clone(), name.to_owned()))?;
        Some((module, name.to_owned(), role))
    }
}

fn swift_field_name(s: &str) -> String {
    if s.is_empty()
        || !s
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return format!("f_{s}");
    }
    if is_swift_keyword(s) {
        format!("`{s}`")
    } else {
        s.to_owned()
    }
}

/// `true` for the Swift reserved words a Kio field / case key could
/// collide with (the keywords legal as a bare identifier need escaping).
pub(crate) fn is_swift_keyword(s: &str) -> bool {
    matches!(
        s,
        "associatedtype"
            | "class"
            | "deinit"
            | "enum"
            | "extension"
            | "fileprivate"
            | "func"
            | "import"
            | "init"
            | "inout"
            | "internal"
            | "let"
            | "open"
            | "operator"
            | "private"
            | "protocol"
            | "public"
            | "rethrows"
            | "static"
            | "struct"
            | "subscript"
            | "typealias"
            | "var"
            | "break"
            | "case"
            | "continue"
            | "default"
            | "defer"
            | "do"
            | "else"
            | "fallthrough"
            | "for"
            | "guard"
            | "if"
            | "in"
            | "repeat"
            | "return"
            | "switch"
            | "where"
            | "while"
            | "as"
            | "catch"
            | "false"
            | "is"
            | "nil"
            | "super"
            | "self"
            | "Self"
            | "throw"
            | "throws"
            | "true"
            | "try"
            | "Any"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forall_owner_names_case_components_before_role_framing() {
        let constructor = |newtype: &str, member: &str| {
            swift_forall_owner_name(&BoundaryFacadeSiteOwner::NewtypeConstructor {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            })
        };
        assert_eq!(constructor("A_b", "c"), "NewtypeCtor_uAB__c");
        assert_eq!(constructor("A", "b_c"), "NewtypeCtor_uA__bC");
        assert_eq!(constructor("A_", "_b"), "NewtypeCtor_uA_u___ub");
        assert_ne!(constructor("A_", "b"), constructor("A", "_b"));
        assert_eq!(
            swift_forall_owner_name(&BoundaryFacadeSiteOwner::HostFunction {
                name: "read_word".to_owned(),
            }),
            "HostFn_ureadWord"
        );
    }

    #[test]
    fn swift_field_name_keeps_natural_case_and_escapes_keywords() {
        assert_eq!(swift_field_name("foo"), "foo");
        assert_eq!(swift_field_name("foo_bar"), "foo_bar");
        assert_eq!(swift_field_name("Box"), "Box");
        assert_eq!(swift_field_name("case"), "`case`");
        assert_eq!(swift_field_name("_0"), "_0");
    }
}

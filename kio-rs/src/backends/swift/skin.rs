//! Swift backend — the typed FFI skin.
//!
//! Swift's execution interfaces are erased (`Any`), but the host's contract is
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
//! Canonical native nodes flow unchanged inside `Any`. Products and sums
//! right-nest binary generic nodes; nominal applications use the same unary
//! application identity for saturated and partial uses. Lexical private type
//! witnesses construct those exact nodes. Boundary adapters recover a known
//! native type without traversing host-owned payloads.
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
use crate::backends::boundary_docs;
use crate::backends::boundary_facade::{
    BoundaryFacadeSupportOrigin, BoundaryHostBindingOrigin, BoundaryHostTypeBinding,
    BoundaryNewtypeSurface, FacadeUse, PreparedBoundaryCallableSites, QualifiedTypeName,
};
use crate::backends::public_names::encode_host_identity;
use crate::backends::skin::ExactHostTypeTable;
use crate::pass::resolve::Package;

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
pub(crate) struct PreparedSwiftShapes<'p> {
    package: &'p Package<Routed>,
    host_protocol: String,
    host_bindings: BTreeMap<QualifiedTypeName, PreparedSwiftHostBinding>,
    application_arities: BTreeMap<usize, BoundaryFacadeSupportOrigin>,
    forall_carriers: Mutex<BTreeMap<String, (String, BoundaryFacadeSupportOrigin)>>,
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

impl<'p> PreparedSwiftShapes<'p> {
    pub(crate) fn new(
        prepared: &PreparedBoundaryCallableSites,
        host_protocol: &str,
        package: &'p Package<Routed>,
    ) -> Self {
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
            package,
            host_protocol: host_protocol.to_owned(),
            host_bindings,
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

    pub(crate) fn package(&self) -> &'p Package<Routed> {
        self.package
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

    pub(crate) fn register_forall(
        &self,
        name: String,
        declaration: String,
        origin: BoundaryFacadeSupportOrigin,
    ) {
        let mut carriers = self
            .forall_carriers
            .lock()
            .expect("Swift forall-carrier registry mutex is not poisoned");
        if let Some((previous, previous_origin)) = carriers.get_mut(&name) {
            assert_eq!(
                *previous, declaration,
                "one exact Swift forall carrier received two declarations: {name}"
            );
            *previous_origin = join_support_origin(*previous_origin, origin);
        } else {
            carriers.insert(name, (declaration, origin));
        }
    }

    pub(crate) fn has_forall(&self, name: &str, origin: BoundaryFacadeSupportOrigin) -> bool {
        self.forall_carriers
            .lock()
            .expect("Swift forall-carrier registry mutex is not poisoned")
            .get(name)
            // A stronger origin must also reach the residual's nested carriers.
            .is_some_and(|(_, previous)| join_support_origin(*previous, origin) == *previous)
    }

    pub(crate) fn render_shapes(
        &self,
        package: &Package<Routed>,
        prepared: &PreparedBoundaryCallableSites,
    ) -> String {
        let mut out = String::from("// Generated by kio — do not edit by hand.\n");
        for binding in self
            .host_bindings
            .values()
            .filter(|binding| binding.type_arity > 0)
        {
            let comment = if matches!(binding.origin, BoundaryHostBindingOrigin::Live) {
                let selected = prepared
                    .host_binding(&binding.name)
                    .expect("a live Swift carrier belongs to the prepared boundary");
                boundary_docs::host_type(package, selected)
            } else {
                None
            };
            render_parameterized_host_carrier(binding, comment, &mut out);
        }
        for entry in prepared.public_newtypes() {
            render_prepared_newtype_carrier(
                entry,
                &self.host_protocol,
                None,
                boundary_docs::newtype(package, entry),
                &mut out,
            );
        }
        for entry in prepared.retained_public_newtypes() {
            render_prepared_newtype_carrier(
                entry,
                &self.host_protocol,
                prepared.retained_public_newtype_removed_at(entry.name()),
                None,
                &mut out,
            );
        }
        let public_names = prepared
            .public_newtypes()
            .chain(prepared.retained_public_newtypes())
            .map(|entry| entry.name().clone())
            .collect::<BTreeSet<_>>();
        for (module, entry) in package.modules() {
            for item in &entry.module.items {
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    if let Some(declaration) = declaration.newtype() {
                        let name = QualifiedTypeName::new(
                            module.split('/').map(str::to_owned).collect(),
                            declaration.name.clone(),
                        )
                        .expect("a routed newtype has an exact qualified identity");
                        if !public_names.contains(&name) {
                            render_newtype_representation(
                                &name,
                                declaration.type_params.len(),
                                &self.host_protocol,
                                false,
                                "",
                                &mut out,
                            );
                        }
                    }
                });
            }
        }
        for (name, (declaration, origin)) in self
            .forall_carriers
            .lock()
            .expect("Swift forall-carrier registry mutex is not poisoned")
            .iter()
        {
            if let Some(version) = origin.removed_at_version() {
                let message = format!(
                    "Kio callable carrier `{name}` is retained only for removed host declarations from contract v{version}"
                );
                render_deprecated_callable(declaration, &message, &mut out);
            } else {
                out.push_str(declaration);
            }
        }
        for (arity, origin) in &self.application_arities {
            render_application_carrier(*arity, *origin, &mut out);
        }
        out
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

fn render_parameterized_host_carrier(
    binding: &PreparedSwiftHostBinding,
    comment: Option<&crate::ast::DocComment>,
    out: &mut String,
) {
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
        "\n{deprecated}public enum {native_identity} {{}}\n{deprecated}public typealias {marker} = KioNative<{native_identity}>\n\n"
    ));
    super::emit::push_doc(out, comment, "");
    let mut prefix = marker.clone();
    for parameter in parameters.iter().take(parameters.len() - 1) {
        prefix = format!("KioApply1<{prefix}, {parameter}>");
    }
    let applied = format!("KioApply1<{prefix}, {}>", parameters.last().unwrap());
    let mut method_params = parameters[..parameters.len() - 1].to_vec();
    method_params.push("Storage".to_owned());
    let method_generic = swift_generic_declaration(&method_params);
    out.push_str(&format!(
        "{deprecated}public typealias {name}{declaration} = {applied}\n\nextension KioApply1 {{\n{member_deprecated}\tpublic init{method_generic}(_ value: Storage) where F == {prefix} {{ self.__kioValue = value }}\n{member_deprecated}\tpublic func value{method_generic}(as _: Storage.Type = Storage.self) -> Storage where F == {prefix} {{ __kioChecked(self.__kioValue, as: Storage.self) }}\n}}\n"
    ));
}

fn render_prepared_newtype_carrier(
    entry: &crate::backends::boundary_facade::BoundaryPublicNewtypeInventoryEntry,
    host_protocol: &str,
    retained_at_version: Option<u32>,
    comment: Option<&crate::ast::DocComment>,
    out: &mut String,
) {
    let deprecated = retained_at_version
        .map(|version| {
            swift_deprecated_attribute(&format!(
                "Kio newtype `{}.{}` is retained only for removed host declarations from contract v{version}",
                entry.name().module_segments().join("."),
                entry.name().name()
            ))
        })
        .unwrap_or_default();
    let surface = match entry.surface() {
        BoundaryNewtypeSurface::Unexposed => "unexposed",
        BoundaryNewtypeSurface::Opaque => "opaque",
        BoundaryNewtypeSurface::Constructor { .. } => "constructor",
        BoundaryNewtypeSurface::Projector { .. } => "projector",
        BoundaryNewtypeSurface::Both { .. } => "constructor and projector",
    };
    out.push('\n');
    let before = out.len();
    super::emit::push_doc(out, comment, "");
    let prefix = if out.len() != before {
        out.push_str("///\n");
        "///"
    } else {
        "//"
    };
    out.push_str(&format!(
        "{prefix} Exact {surface} carrier for Kio newtype `{}.{}`.\n",
        entry.name().module_segments().join("."),
        entry.name().name()
    ));
    render_newtype_representation(
        entry.name(),
        entry.type_params().len(),
        host_protocol,
        true,
        &deprecated,
        out,
    );
}

fn render_newtype_representation(
    name: &QualifiedTypeName,
    arity: usize,
    host_protocol: &str,
    public: bool,
    deprecated: &str,
    out: &mut String,
) {
    let access = if public { "public " } else { "" };
    let carrier = swift_newtype_carrier_name(name);
    let marker = swift_newtype_constructor_name(name);
    if arity == 0 {
        out.push_str(&format!("{deprecated}{access}struct {carrier}<H: {host_protocol}>: __KioNominalValue {{\n    let __kioValue: Any\n    init(_ value: Any) {{ self.__kioValue = value }}\n}}\nprivate struct __KioWitness_{carrier}<H: {host_protocol}>: __KioNominalWitness {{\n    typealias Native = {carrier}<H>\n    func construct(_ payload: Any) -> Any {{ Native(payload) }}\n}}\nfunc __kioWitness_{carrier}<H: {host_protocol}>(_: H.Type) -> any __KioTypeWitness {{ __KioWitness_{carrier}<H>() }}\n"));
    } else {
        out.push_str(&format!(
            "{deprecated}{access}enum {marker}<H: {host_protocol}> {{}}\n"
        ));
        let mut parameters = vec![format!("H: {host_protocol}")];
        let mut applied = format!("{marker}<H>");
        for index in 0..arity {
            parameters.push(format!("T{index}"));
            applied = format!("KioApply1<{applied}, T{index}>");
        }
        out.push_str(&format!(
            "{deprecated}{access}typealias {carrier}<{}> = {applied}\n",
            parameters.join(", ")
        ));
    }
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
        .and_modify(|existing| *existing = join_support_origin(*existing, incoming))
        .or_insert(incoming);
}

fn join_support_origin(
    existing: BoundaryFacadeSupportOrigin,
    incoming: BoundaryFacadeSupportOrigin,
) -> BoundaryFacadeSupportOrigin {
    match (existing, incoming) {
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
    }
}

fn render_deprecated_callable(declaration: &str, message: &str, out: &mut String) {
    for line in declaration.lines() {
        let member = line.trim_start();
        if member.starts_with("public ")
            || member.starts_with("associatedtype ")
            || member.starts_with("func specialize<")
        {
            out.push_str(&swift_optional_deprecated_attribute(
                Some(message),
                &line[..line.len() - member.len()],
            ));
        }
        out.push_str(line);
        out.push('\n');
    }
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
    if arity == 1 {
        return;
    }
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
    let mut applied = "F".to_owned();
    for index in 0..arity {
        applied = format!("KioApply1<{applied}, T{index}>");
    }
    out.push_str(&format!(
        "\n{deprecated}public typealias {name}{declaration} = {applied}\n"
    ));
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
mod support_tests {
    use super::*;

    #[test]
    fn live_support_dominates_history_in_either_visit_order() {
        let early = BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: 2,
        };
        let late = BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: 7,
        };
        assert_eq!(join_support_origin(early, late), early);
        assert_eq!(join_support_origin(late, early), early);
        for retained in [early, late] {
            assert_eq!(
                join_support_origin(retained, BoundaryFacadeSupportOrigin::Live),
                BoundaryFacadeSupportOrigin::Live
            );
            assert_eq!(
                join_support_origin(BoundaryFacadeSupportOrigin::Live, retained),
                BoundaryFacadeSupportOrigin::Live
            );
        }
    }

    #[test]
    fn retained_callable_marks_public_members_without_marking_private_witnesses() {
        let declaration = "public protocol CarrierImplementation {\n    associatedtype H\n    func specialize<T>(_: T.Type) -> T\n}\npublic struct Carrier {\n    public init() {}\n    func __kioAdvance() {}\n}\nprivate struct Witness {}\n";
        let mut out = String::new();
        render_deprecated_callable(declaration, "retained", &mut out);
        assert_eq!(out.matches("@available(*, deprecated").count(), 5);
        assert!(
            out.contains(
                "    @available(*, deprecated, message: \"retained\")\n    func specialize"
            )
        );
        assert!(out.contains("    public init() {}\n    func __kioAdvance() {}"));
        assert!(out.ends_with("}\nprivate struct Witness {}\n"));
    }
}

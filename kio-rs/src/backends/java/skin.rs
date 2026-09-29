//! Java realization of the backend-neutral prepared boundary inventory.
//!
//! Exact host declarations remain independent package-root bindings, while
//! products and sums reuse the planner's payload-generic semantic shells.

use std::collections::BTreeMap;

use crate::ast::{Role, Routed};
use crate::backends::boundary_facade::{
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSupportOrigin,
    BoundaryHostBindingOrigin, BoundaryHostTypeBinding, BoundaryNominalDeclaration, FacadeShellId,
    PreparedBoundaryCallableOriginRef, PreparedBoundaryCallableSites, QualifiedTypeName,
    SemanticKey,
};
use crate::backends::public_names::encode_host_identity;
use crate::pass::resolve::Package;

pub(crate) const JAVA_INSTANCE_REFERENCE_SLOT_LIMIT: usize = 254;

/// The private interpreter body type selected for a `role(r)` binding.
pub(crate) fn role_to_java_type(role: Role) -> &'static str {
    match role {
        Role::I8 => "byte",
        Role::I16 => "short",
        Role::I32 => "int",
        Role::I64 => "long",
        Role::U8 => "short",
        Role::U16 => "int",
        Role::U32 => "long",
        Role::I128 | Role::U64 | Role::U128 => "java.math.BigInteger",
        Role::F32 => "float",
        Role::F64 => "double",
        Role::Bool => "boolean",
        Role::Str => "String",
    }
}

/// One exact declaration-keyed host binding in Java's package-root generic
/// surface. Nullary declarations become ordinary Java type parameters;
/// parameterized declarations use a declaration-owned carrier because Java
/// has no higher-kinded type-parameter syntax.
#[derive(Debug, Clone)]
pub(crate) struct JavaHostBinding {
    pub name: QualifiedTypeName,
    pub type_arity: usize,
    pub role: Option<Role>,
    pub retained_at: Option<u32>,
}

impl JavaHostBinding {
    pub(crate) fn type_parameter(&self) -> Option<String> {
        (self.type_arity == 0).then(|| java_host_type_parameter(&self.name))
    }

    pub(crate) fn carrier(&self) -> String {
        java_host_type_carrier(&self.name)
    }

    pub(crate) fn adapter_from_body(&self) -> String {
        format!(
            "KioHostBinding_{}_fromBody",
            java_host_type_identity(&self.name)
        )
    }

    pub(crate) fn adapter_to_body(&self) -> String {
        format!(
            "KioHostBinding_{}_toBody",
            java_host_type_identity(&self.name)
        )
    }
}

pub(crate) fn java_host_type_parameter(name: &QualifiedTypeName) -> String {
    format!("T_{}", java_host_type_identity(name))
}

pub(crate) fn java_host_type_carrier(name: &QualifiedTypeName) -> String {
    format!("KioHostType_{}", java_host_type_identity(name))
}

pub(crate) fn java_host_type_identity(name: &QualifiedTypeName) -> String {
    format!(
        "{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

/// Exact Java boundary inventory selected by the shared planner.
pub(crate) struct JavaShapes<'p> {
    _package: &'p Package<Routed>,
    host_bindings: BTreeMap<(String, String), JavaHostBinding>,
    has_live_host_binding: bool,
    live_root_type_parameters: Vec<String>,
    newtype_payload_owners: JavaNewtypePayloadOwners,
    facade_shells: BTreeMap<FacadeShellId, BoundaryFacadeSupportOrigin>,
}

struct JavaNewtypePayloadOwners {
    by_name: BTreeMap<QualifiedTypeName, JavaNewtypePayloadOwner>,
    by_plan: BTreeMap<usize, QualifiedTypeName>,
    #[cfg(all(test, feature = "surface"))]
    index_construction_visits: usize,
}

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JavaPayloadOwnerIndexMeasurement {
    pub(crate) owner_count: usize,
    pub(crate) index_construction_visits: usize,
    pub(crate) indexed_lookups: usize,
    pub(crate) legacy_candidate_visits: usize,
    pub(crate) identity_parity_checks: usize,
}

#[derive(Clone)]
pub(crate) struct JavaNewtypePayloadOwner {
    pub(crate) site: BoundaryFacadeSiteId,
    pub(crate) origin: BoundaryFacadeSupportOrigin,
    plan: usize,
}

fn join_support_origin(
    current: BoundaryFacadeSupportOrigin,
    incoming: BoundaryFacadeSupportOrigin,
) -> BoundaryFacadeSupportOrigin {
    match (current, incoming) {
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

impl<'p> JavaShapes<'p> {
    pub(crate) fn new(
        package: &'p Package<Routed>,
        prepared: &PreparedBoundaryCallableSites,
    ) -> Self {
        let host_bindings: BTreeMap<(String, String), JavaHostBinding> = prepared
            .host_bindings()
            .map(|binding| {
                let name = binding.name().clone();
                let role = match binding.binding() {
                    BoundaryHostTypeBinding::Role(role) => Some(role),
                    BoundaryHostTypeBinding::Roleless => None,
                };
                let retained_at = match binding.origin() {
                    BoundaryHostBindingOrigin::Live => None,
                    BoundaryHostBindingOrigin::Retained { removed_at_version } => {
                        Some(removed_at_version)
                    }
                };
                (
                    (name.module_segments().join("/"), name.name().to_owned()),
                    JavaHostBinding {
                        name,
                        type_arity: binding.type_params().len(),
                        role,
                        retained_at,
                    },
                )
            })
            .collect();
        let has_live_host_binding = host_bindings
            .values()
            .any(|binding| binding.retained_at.is_none());
        // Retained host types cannot remain Java root generics without forcing
        // current hosts to select them; see `specs/backends/java.md`
        // § Host-type removal cannot preserve generic arity.
        let live_root_type_parameters = host_bindings
            .values()
            .filter(|binding: &&JavaHostBinding| binding.retained_at.is_none())
            .filter_map(|binding| binding.type_parameter())
            .collect();
        // Shared preparation has already admitted only semantically compatible
        // epochs. Select one deterministic owner per exact transparent
        // newtype, with live provenance replacing retained provenance, and
        // index the selected plan for direct forall ownership lookup.
        let mut newtype_payload_owners = BTreeMap::new();
        for site in prepared.sites() {
            let incoming_origin = match site.origin() {
                PreparedBoundaryCallableOriginRef::Live(_) => BoundaryFacadeSupportOrigin::Live,
                PreparedBoundaryCallableOriginRef::Retained(metadata) => {
                    BoundaryFacadeSupportOrigin::Retained {
                        removed_at_version: metadata.removed_at_version(),
                    }
                }
            };
            for (name, declaration) in site.nominals().declarations() {
                let BoundaryNominalDeclaration::Newtype {
                    transparent_payload: Some(payload),
                    ..
                } = declaration
                else {
                    continue;
                };
                let incoming = JavaNewtypePayloadOwner {
                    site: site.site().clone(),
                    origin: incoming_origin,
                    plan: payload.facade() as *const BoundaryFacadePlan as usize,
                };
                newtype_payload_owners
                    .entry(name.clone())
                    .and_modify(|owner: &mut JavaNewtypePayloadOwner| {
                        let replace = owner.origin.removed_at_version().is_some()
                            && incoming_origin.removed_at_version().is_none();
                        owner.origin = join_support_origin(owner.origin, incoming_origin);
                        if replace {
                            owner.site = incoming.site.clone();
                            owner.plan = incoming.plan;
                        }
                    })
                    .or_insert(incoming);
            }
        }
        #[cfg(all(test, feature = "surface"))]
        let newtype_payload_index_construction_visits = std::cell::Cell::new(0);
        let newtype_payload_plan_owners = newtype_payload_owners
            .iter()
            .map(|(name, owner)| {
                #[cfg(all(test, feature = "surface"))]
                newtype_payload_index_construction_visits
                    .set(newtype_payload_index_construction_visits.get() + 1);
                (owner.plan, name.clone())
            })
            .collect();
        let newtype_payload_owners = JavaNewtypePayloadOwners {
            by_name: newtype_payload_owners,
            by_plan: newtype_payload_plan_owners,
            #[cfg(all(test, feature = "surface"))]
            index_construction_visits: newtype_payload_index_construction_visits.get(),
        };
        let mut facade_shells = prepared
            .shell_origins()
            .map(|(shell, origin)| (shell.clone(), origin))
            .collect::<BTreeMap<_, _>>();
        facade_shells.extend(
            prepared
                .shell_origins()
                .filter(|(shell, _)| {
                    shell.kind() == crate::backends::boundary_facade::FacadeKind::Sum
                        && shell.ordered_keys().len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT
                })
                .map(|(shell, origin)| (shell.product_companion(), origin)),
        );
        Self {
            _package: package,
            host_bindings,
            has_live_host_binding,
            live_root_type_parameters,
            newtype_payload_owners,
            facade_shells,
        }
    }

    pub(crate) fn host_bindings(&self) -> impl Iterator<Item = &JavaHostBinding> {
        self.host_bindings.values()
    }

    pub(crate) fn live_root_type_parameters(&self) -> &[String] {
        &self.live_root_type_parameters
    }

    pub(crate) fn has_live_host_binding(&self) -> bool {
        self.has_live_host_binding
    }

    pub(crate) fn newtype_payload_owner(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&JavaNewtypePayloadOwner> {
        self.newtype_payload_owners.by_name.get(name)
    }

    pub(crate) fn newtype_payload_owners(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&QualifiedTypeName, &JavaNewtypePayloadOwner)>
    + ExactSizeIterator {
        self.newtype_payload_owners.by_name.iter()
    }

    pub(crate) fn newtype_payload_plan_owner(
        &self,
        plan: &BoundaryFacadePlan,
    ) -> Option<&QualifiedTypeName> {
        self.newtype_payload_owners
            .by_plan
            .get(&(plan as *const BoundaryFacadePlan as usize))
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn measure_newtype_payload_plan_owner_index(
        &self,
        prepared: &PreparedBoundaryCallableSites,
    ) -> JavaPayloadOwnerIndexMeasurement {
        let owners = &self.newtype_payload_owners;
        let mut indexed_lookups = 0;
        let mut legacy_candidate_visits = 0;
        let mut identity_parity_checks = 0;

        for (expected_name, expected_owner) in &owners.by_name {
            let site = prepared
                .site(&expected_owner.site)
                .expect("a selected Java payload owner keeps its prepared site");
            let plan = site
                .nominals()
                .declarations()
                .find_map(|(name, declaration)| {
                    if name != expected_name {
                        return None;
                    }
                    match declaration {
                        BoundaryNominalDeclaration::Newtype {
                            transparent_payload: Some(payload),
                            ..
                        } => Some(payload.facade()),
                        _ => None,
                    }
                })
                .expect("a selected Java payload owner keeps its exact payload plan");
            assert_eq!(
                plan as *const BoundaryFacadePlan as usize, expected_owner.plan,
                "the selected Java payload owner changed plan identity"
            );

            let indexed = self
                .newtype_payload_plan_owner(plan)
                .expect("the Java reverse index contains every selected payload plan");
            indexed_lookups += 1;
            let stored = owners
                .by_plan
                .get(&expected_owner.plan)
                .expect("the Java reverse index contains the selected plan key");
            assert!(
                std::ptr::eq(indexed, stored),
                "the production Java lookup did not return the reverse-index entry"
            );

            let legacy = owners
                .by_name
                .iter()
                .find_map(|(candidate_name, candidate_owner)| {
                    legacy_candidate_visits += 1;
                    (candidate_owner.plan == expected_owner.plan).then_some(candidate_name)
                })
                .expect("the legacy Java scan finds every selected payload plan");
            assert_eq!(indexed, expected_name);
            assert_eq!(legacy, expected_name);
            identity_parity_checks += 1;
        }

        JavaPayloadOwnerIndexMeasurement {
            owner_count: owners.by_name.len(),
            index_construction_visits: owners.index_construction_visits,
            indexed_lookups,
            legacy_candidate_visits,
            identity_parity_checks,
        }
    }

    pub(crate) fn facade_shells(&self) -> impl Iterator<Item = &FacadeShellId> {
        self.facade_shells.keys()
    }

    pub(crate) fn facade_shell_origins(
        &self,
    ) -> impl Iterator<Item = (&FacadeShellId, BoundaryFacadeSupportOrigin)> {
        self.facade_shells
            .iter()
            .map(|(shell, origin)| (shell, *origin))
    }

    pub(crate) fn host_binding(&self, name: &QualifiedTypeName) -> Option<&JavaHostBinding> {
        self.host_bindings
            .get(&(name.module_segments().join("/"), name.name().to_owned()))
    }
}

/// Pure Java member rendering of one planner-selected semantic key. Qualified
/// and positional roles use disjoint reserved classes, so field/variant names
/// never depend on declaration order or namespace occupancy.
pub(crate) fn java_semantic_member(key: &SemanticKey) -> String {
    match key {
        SemanticKey::Bare { name } => {
            sanitize_java_member(&crate::backends::public_names::host_name_core(name))
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

/// Fold a semantic key into a legal Java identifier.
pub(crate) fn sanitize_java_member(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out = format!("_{out}");
    }
    if crate::backends::namespace::validate_java_namespace(&out).is_err() {
        out.push('_');
    }
    out
}

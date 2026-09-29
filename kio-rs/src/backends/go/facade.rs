//! Pure Go realization of backend-neutral boundary facades.
//!
//! The catalog in this module consumes only the already-prepared semantic
//! boundary catalog. It neither scans `Package`/`Type` nor consults the Go
//! emitter's namespace occupancy. Each callable keeps its own nominal
//! snapshot, including retained snapshots, while completed shell and carrier
//! identities may be unioned after realization.
//!
//! Transparent newtype arguments remain terms until their declaration
//! binders are demanded. This is load-bearing for higher-kinded substitution,
//! for unused recursive arguments, and for collapsing one recursive `Both`
//! application as a whole instead of rendering a partially expanded shape.
//!
//! Names are selected before namespace validation. [`GoFacadeCatalog::claims`]
//! covers this module's package-level and field-level declarations, but does
//! not establish that the whole emitted package is collision-free. The
//! package compositor must merge these claims with runtime, emitter, source,
//! and stable-alias claims and validate that complete immutable inventory
//! before calling [`GoFacadeCatalog::render_declarations`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::sync::Arc;

use crate::ast::Role;
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryCallablePlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryHostBinding,
    BoundaryHostBindingOrigin, BoundaryHostTypeBinding, BoundaryNewtypePayloadPlan,
    BoundaryNewtypeSurface, BoundaryNominalDeclaration, BoundaryNominalTypeParam,
    BoundaryPublicNewtypeInventoryEntry, CallableExecutionLayout, CallableExecutionStage,
    CallableSourceParamAdapter, CallableTypeStageAction, CallableValueStageLayout, FacadeBinder,
    FacadeBinderId, FacadeKind, FacadeShellId, FacadeUse, FacadeUseId,
    PreparedBoundaryCallableSite, PreparedBoundaryCallableSites, QualifiedTypeName, SemanticKey,
};
use crate::backends::public_names::readable_role_adapter_identity;

use super::naming::{
    GoIdentifier, GoSemanticKeyRole, ScopeClaims, encode_semantic_key, facade_shell_go_identifier,
};

const NEWTYPE_CODEC_VERSION: u64 = 1;
const SUM_USE_CODEC_VERSION: u64 = 1;

const UNIT_TYPE: &str = "Unit";
const ERASED_TYPE: &str = "any";
const SUM_CARRIER_TYPE: &str = "KioSum";
const SUM_CASE_MARKER: &str = "kioCaseMarker";
const SUM_ROW_CONSTRAINT: &str = "kioSumRow";
const SUM_VALUE_ACCESSOR: &str = "Value";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GoDeclarationProvenance {
    Live,
    Retained { removed_at_version: u32 },
}

impl GoDeclarationProvenance {
    fn from_host_binding(origin: BoundaryHostBindingOrigin) -> Self {
        match origin {
            BoundaryHostBindingOrigin::Live => Self::Live,
            BoundaryHostBindingOrigin::Retained { removed_at_version } => {
                Self::Retained { removed_at_version }
            }
        }
    }

    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Live, _) | (_, Self::Live) => Self::Live,
            (
                Self::Retained {
                    removed_at_version: left,
                },
                Self::Retained {
                    removed_at_version: right,
                },
            ) => Self::Retained {
                removed_at_version: left.min(right),
            },
        }
    }

    fn removed_at_version(self) -> Option<u32> {
        match self {
            Self::Live => None,
            Self::Retained { removed_at_version } => Some(removed_at_version),
        }
    }
}

fn record_declaration_provenance<K: Clone + Ord>(
    provenance: &mut BTreeMap<K, GoDeclarationProvenance>,
    key: &K,
    incoming: GoDeclarationProvenance,
) {
    provenance
        .entry(key.clone())
        .and_modify(|existing| *existing = existing.join(incoming))
        .or_insert(incoming);
}

fn render_deprecation_comment(out: &mut String, removed_at_version: Option<u32>) {
    render_deprecation_comment_with_indent(out, removed_at_version, "");
}

fn render_deprecation_comment_with_indent(
    out: &mut String,
    removed_at_version: Option<u32>,
    indent: &str,
) {
    if let Some(version) = removed_at_version {
        writeln!(
            out,
            "{indent}// Deprecated: retained only for source compatibility with host declarations removed at signature v{version}."
        )
        .expect("writing to String cannot fail");
    }
}

/// Failure to realize one prepared semantic facade as Go syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoFacadeError {
    message: String,
}

impl GoFacadeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GoFacadeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GoFacadeError {}

/// One rendered Go boundary type.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GoType(String);

impl GoType {
    fn new(spelling: impl Into<String>) -> Self {
        Self(spelling.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for GoType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Namespace scopes owned by the facade realization.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoFacadeScope {
    /// Go's one package-block namespace for types, functions, variables, and
    /// constants.
    Package,
    /// Exported fields of one generic product shell.
    ProductFields(FacadeShellId),
}

/// Fixed package declarations/reservations owned by the Go facade.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoFacadeSupportName {
    Unit,
    SumCarrier,
    SumCaseMarker,
    SumRowConstraint,
}

/// Role of one identity-derived newtype carrier name.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoCarrierNameRole {
    PublicType,
}

/// Role of one package-level declaration derived from a sum key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoSumKeyNameRole {
    SlotWitness,
    RowConstraint,
    PublicCase,
    PrivateCase,
    Constructor,
}

/// Role of one package-level declaration derived from a structural shell.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoShellNameRole {
    Shell,
    SumRow,
}

/// Semantic owner used while validating the facade's immutable claims.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoFacadeNameOwner {
    Support(GoFacadeSupportName),
    Shell {
        shell: FacadeShellId,
        role: GoShellNameRole,
    },
    ProductField {
        shell: FacadeShellId,
        key: SemanticKey,
    },
    SumKey {
        key: SemanticKey,
        role: GoSumKeyNameRole,
    },
    Carrier {
        name: QualifiedTypeName,
        role: GoCarrierNameRole,
    },
    ContextualSumRow {
        site: BoundaryFacadeSiteId,
        ordinal: u64,
    },
}

pub(crate) type GoFacadeClaims = ScopeClaims<GoFacadeScope, GoIdentifier, GoFacadeNameOwner>;

/// Why a contextual facade use has the erased Go boundary type.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoErasureReason {
    Bottom,
    UnboundType,
    HigherKindedApplication,
    RolelessHost(QualifiedTypeName),
    Existential,
    RecursiveBoth(QualifiedTypeName),
}

/// Exact source arena and use ID behind a contextual facade cursor.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct GoFacadeUseSource {
    arena: GoFacadeArenaId,
    use_id: FacadeUseId,
}

impl GoFacadeUseSource {
    pub(crate) fn arena(&self) -> &GoFacadeArenaId {
        &self.arena
    }

    pub(crate) fn use_id(&self) -> FacadeUseId {
        self.use_id
    }
}

/// Site-local semantic arena identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoFacadeArenaId {
    Root,
    TransparentPayload(QualifiedTypeName),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GoFacadeTerm {
    Use {
        source: GoFacadeUseSource,
        substitutions: Arc<BTreeMap<FacadeBinderId, GoFacadeContext>>,
    },
    Erased(GoErasureReason),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GoFacadeContext {
    term: Arc<GoFacadeTerm>,
}

impl GoFacadeContext {
    fn use_in(
        arena: GoFacadeArenaId,
        use_id: FacadeUseId,
        substitutions: Arc<BTreeMap<FacadeBinderId, GoFacadeContext>>,
    ) -> Self {
        Self {
            term: Arc::new(GoFacadeTerm::Use {
                source: GoFacadeUseSource { arena, use_id },
                substitutions,
            }),
        }
    }

    fn erased(reason: GoErasureReason) -> Self {
        Self {
            term: Arc::new(GoFacadeTerm::Erased(reason)),
        }
    }
}

/// Cursor-local expansion state. Unlike [`GoFacadeContext`], this never
/// participates in semantic identity or deterministic row ordering.
#[derive(Clone, Debug)]
struct GoFacadeTraversal {
    context: GoFacadeContext,
    bound_args: Arc<BTreeMap<FacadeBinderId, GoFacadeTraversal>>,
    active_newtypes: Arc<Vec<QualifiedTypeName>>,
}

impl GoFacadeTraversal {
    fn erased(reason: GoErasureReason, active_newtypes: Arc<Vec<QualifiedTypeName>>) -> Self {
        Self {
            context: GoFacadeContext::erased(reason),
            bound_args: Arc::new(BTreeMap::new()),
            active_newtypes,
        }
    }
}

/// Identity-derived, non-generic carrier for one exact nominal declaration.
///
/// Type-parameter arity belongs to a site-local application, not to this
/// declaration. This lets live and retained epochs safely share the same
/// carrier declaration whenever their exact qualified identity agrees.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct GoNewtypeCarrier {
    name: QualifiedTypeName,
    public_type: GoIdentifier,
    identity_field: GoIdentifier,
    value_accessor: GoIdentifier,
}

impl GoNewtypeCarrier {
    fn new(name: &QualifiedTypeName) -> Result<Self, GoFacadeError> {
        let frame = encode_qualified_type_frame(name);
        Ok(Self {
            name: name.clone(),
            public_type: go_identifier(format!("KioNewtype_{frame}"))?,
            identity_field: go_identifier(format!("kioNewtypeMarker_{frame}"))?,
            value_accessor: go_identifier(format!("kioNewtypeValue_{frame}"))?,
        })
    }

    pub(crate) fn name(&self) -> &QualifiedTypeName {
        &self.name
    }

    pub(crate) fn public_type(&self) -> &GoIdentifier {
        &self.public_type
    }

    /// The concrete type used to construct hidden storage inside the emitted
    /// package. It is the public nominal type itself, not an interface-backed
    /// implementation that an external embedding could forge.
    #[cfg(test)]
    pub(crate) fn storage_type(&self) -> &GoIdentifier {
        &self.public_type
    }

    #[cfg(test)]
    pub(crate) fn value_accessor(&self) -> &GoIdentifier {
        &self.value_accessor
    }

    pub(crate) fn render_out(&self, value: &str) -> String {
        format!("{}{{value: {value}}}", self.public_type)
    }

    pub(crate) fn render_in(&self, value: &str) -> String {
        format!("({value}).{}()", self.value_accessor)
    }

    #[cfg(test)]
    fn render_declaration(&self, out: &mut String) {
        self.render_declaration_with_provenance(out, GoDeclarationProvenance::Live);
    }

    fn render_declaration_with_provenance(
        &self,
        out: &mut String,
        provenance: GoDeclarationProvenance,
    ) {
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(out, "type {} struct {{", self.public_type)
            .expect("writing to String cannot fail");
        writeln!(out, "\t{} [0]struct{{}}", self.identity_field)
            .expect("writing to String cannot fail");
        writeln!(out, "\tvalue any").expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(
            out,
            "func (value {}) {}() any {{",
            self.public_type, self.value_accessor
        )
        .expect("writing to String cannot fail");
        writeln!(out, "\treturn value.value").expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GoSumKey {
    key: SemanticKey,
    slot_witness: GoIdentifier,
    slot_acceptor: GoIdentifier,
    row_constraint: GoIdentifier,
    public_case: GoIdentifier,
    private_case: GoIdentifier,
    case_marker: GoIdentifier,
    constructor: GoIdentifier,
}

impl GoSumKey {
    fn new(key: &SemanticKey) -> Result<Self, GoFacadeError> {
        let encoded = go_identifier(encode_semantic_key(GoSemanticKeyRole::SumArm, key))?;
        Ok(Self {
            key: key.clone(),
            slot_witness: go_identifier(format!("kioSum_{encoded}_Slot"))?,
            slot_acceptor: go_identifier(format!("kioSum_{encoded}_SlotAccept"))?,
            row_constraint: go_identifier(format!("kioSum_{encoded}_Row"))?,
            public_case: go_identifier(format!("KioSum_{encoded}_Case"))?,
            private_case: go_identifier(format!("kioSum_{encoded}_CaseValue"))?,
            case_marker: go_identifier(format!("kioSum_{encoded}_CaseMarker"))?,
            constructor: go_identifier(format!("NewKioSum_{encoded}"))?,
        })
    }

    #[cfg(test)]
    fn render_declaration(&self, out: &mut String) {
        self.render_declaration_with_provenance(out, GoDeclarationProvenance::Live);
    }

    fn render_declaration_with_provenance(
        &self,
        out: &mut String,
        provenance: GoDeclarationProvenance,
    ) {
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(out, "type {}[P any] struct {{}}\n", self.slot_witness)
            .expect("writing to String cannot fail");
        writeln!(
            out,
            "func ({}[P]) {}(P) {{}}\n",
            self.slot_witness, self.slot_acceptor
        )
        .expect("writing to String cannot fail");
        writeln!(out, "type {}[R, P any] interface {{", self.row_constraint)
            .expect("writing to String cannot fail");
        writeln!(out, "\t{SUM_ROW_CONSTRAINT}[R]").expect("writing to String cannot fail");
        writeln!(out, "\t{}(P)", self.slot_acceptor).expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(out, "type {}[R, P any] interface {{", self.public_case)
            .expect("writing to String cannot fail");
        writeln!(out, "\t{SUM_CASE_MARKER}").expect("writing to String cannot fail");
        writeln!(out, "\t{}(R)", self.case_marker).expect("writing to String cannot fail");
        render_deprecation_comment_with_indent(out, provenance.removed_at_version(), "\t");
        writeln!(out, "\t{SUM_VALUE_ACCESSOR}() P").expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(
            out,
            "type {}[R, P any] struct {{ value P }}\n",
            self.private_case
        )
        .expect("writing to String cannot fail");
        writeln!(
            out,
            "func ({}[R, P]) kioCaseMarker() {{}}\n",
            self.private_case
        )
        .expect("writing to String cannot fail");
        writeln!(
            out,
            "func ({}[R, P]) {}(R) {{}}\n",
            self.private_case, self.case_marker
        )
        .expect("writing to String cannot fail");
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(
            out,
            "func (value {}[R, P]) {SUM_VALUE_ACCESSOR}() P {{",
            self.private_case
        )
        .expect("writing to String cannot fail");
        writeln!(out, "\treturn value.value").expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
        render_deprecation_comment(out, provenance.removed_at_version());
        writeln!(
            out,
            "func {}[R {}[R, P], P any](value P) {SUM_CARRIER_TYPE}[R] {{",
            self.constructor, self.row_constraint
        )
        .expect("writing to String cannot fail");
        writeln!(
            out,
            "\treturn {SUM_CARRIER_TYPE}[R]{{stored: {}[R, P]{{value: value}}}}",
            self.private_case
        )
        .expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
    }
}

/// One reachable generic structural shell and all fixed Go spellings derived
/// from its semantic identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoFacadeShell {
    id: FacadeShellId,
    name: GoIdentifier,
    row_name: Option<GoIdentifier>,
    fields: Vec<GoIdentifier>,
    sum_keys: Vec<GoSumKey>,
}

impl GoFacadeShell {
    fn new(id: &FacadeShellId) -> Result<Self, GoFacadeError> {
        let name = facade_shell_go_identifier(id);
        let mut fields = Vec::new();
        let mut sum_keys = Vec::new();
        match id.kind() {
            FacadeKind::Product => {
                for key in id.ordered_keys() {
                    fields.push(go_identifier(encode_semantic_key(
                        GoSemanticKeyRole::ProductField,
                        key,
                    ))?);
                }
            }
            FacadeKind::Sum => {
                for key in id.ordered_keys() {
                    sum_keys.push(GoSumKey::new(key)?);
                }
            }
        }
        let row_name = match id.kind() {
            FacadeKind::Product => None,
            FacadeKind::Sum => Some(go_identifier(format!("{name}_Row"))?),
        };
        Ok(Self {
            id: id.clone(),
            name,
            row_name,
            fields,
            sum_keys,
        })
    }

    pub(crate) fn id(&self) -> &FacadeShellId {
        &self.id
    }

    #[cfg(test)]
    pub(crate) fn name(&self) -> &GoIdentifier {
        &self.name
    }

    #[cfg(test)]
    pub(crate) fn row_name(&self) -> Option<&GoIdentifier> {
        self.row_name.as_ref()
    }

    pub(crate) fn fields(&self) -> &[GoIdentifier] {
        &self.fields
    }

    fn application(&self, arguments: &[GoType]) -> Result<GoType, GoFacadeError> {
        if arguments.len() != self.id.ordered_keys().len() {
            return Err(GoFacadeError::new(format!(
                "Go facade shell {} expected {} arguments, got {}",
                self.name,
                self.id.ordered_keys().len(),
                arguments.len()
            )));
        }
        Ok(GoType::new(apply_go_type(self.name.as_str(), arguments)))
    }

    fn concrete_row_type(&self, arguments: &[GoType]) -> Result<GoType, GoFacadeError> {
        let row = self
            .row_name
            .as_ref()
            .ok_or_else(|| GoFacadeError::new("a product shell has no concrete sum-row type"))?;
        if arguments.len() != self.sum_keys.len() {
            return Err(GoFacadeError::new(format!(
                "Go sum shell {} expected {} row arguments, got {}",
                self.name,
                self.sum_keys.len(),
                arguments.len()
            )));
        }
        Ok(GoType::new(apply_go_type(row.as_str(), arguments)))
    }
}

fn go_identifier(spelling: String) -> Result<GoIdentifier, GoFacadeError> {
    GoIdentifier::new(spelling).map_err(|error| GoFacadeError::new(error.to_string()))
}

fn apply_go_type(head: &str, arguments: &[GoType]) -> String {
    if arguments.is_empty() {
        head.to_owned()
    } else {
        format!(
            "{head}[{}]",
            arguments
                .iter()
                .map(GoType::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn encode_qualified_type_frame(name: &QualifiedTypeName) -> String {
    let mut encoded = format!(
        "V{NEWTYPE_CODEC_VERSION}_M{}_",
        name.module_segments().len()
    );
    for segment in name.module_segments() {
        write_identity_component(&mut encoded, 'C', segment);
    }
    write_identity_component(&mut encoded, 'N', name.name());
    encoded
}

/// Package-root Go type parameters for exact host-owned declarations.
///
/// The parameter name is a pure function of the exact declaration identity;
/// canonical shared-plan order fixes the parameter order. Only live nullary
/// bindings enter this list: a retained nullary binding is instead a concrete
/// deprecated nominal, so a new host never selects a history-only type.
/// Unrelated non-host declarations cannot perturb any existing public
/// spelling.
#[derive(Clone, Debug, Default)]
struct GoHostBindingPlan {
    parameters: Vec<GoHostBindingParameter>,
    exact_types: BTreeMap<QualifiedTypeName, GoType>,
    role_adapters: BTreeMap<QualifiedTypeName, GoRoleAdapter>,
    parametric_carriers: BTreeMap<QualifiedTypeName, GoHostTypeCarrier>,
    retained_types: BTreeMap<QualifiedTypeName, GoRetainedHostType>,
}

#[derive(Clone, Debug)]
struct GoHostBindingParameter {
    name: GoIdentifier,
    constraint: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct GoRoleAdapter {
    name: QualifiedTypeName,
    boundary_type: GoType,
    internal_type: GoType,
    convert_in: GoIdentifier,
    convert_out: GoIdentifier,
}

#[derive(Clone, Debug)]
pub(crate) struct GoHostTypeCarrier {
    name: QualifiedTypeName,
    arity: usize,
    public_type: GoIdentifier,
    identity_field: GoIdentifier,
    from_native: GoIdentifier,
    provenance: GoDeclarationProvenance,
}

impl GoHostTypeCarrier {
    fn new(
        name: &QualifiedTypeName,
        arity: usize,
        provenance: GoDeclarationProvenance,
    ) -> Result<Self, GoFacadeError> {
        let frame = encode_qualified_type_frame(name);
        Ok(Self {
            name: name.clone(),
            arity,
            public_type: go_identifier(format!("KioHostType_{frame}"))?,
            identity_field: go_identifier(format!("kioHostTypeMarker_{frame}"))?,
            from_native: go_identifier(format!("UnsafeKioHostType_{frame}FromNative"))?,
            provenance,
        })
    }

    pub(crate) fn name(&self) -> &QualifiedTypeName {
        &self.name
    }

    pub(crate) fn public_type(&self) -> &GoIdentifier {
        &self.public_type
    }

    pub(crate) fn native_constructor(&self) -> &GoIdentifier {
        &self.from_native
    }

    fn applied_type(&self, arguments: &[GoType]) -> Result<GoType, GoFacadeError> {
        if arguments.len() != self.arity {
            return Err(GoFacadeError::new(format!(
                "Go host carrier {}.{} expected {} arguments, got {}",
                self.name.module_segments().join("."),
                self.name.name(),
                self.arity,
                arguments.len()
            )));
        }
        Ok(GoType::new(apply_go_type(
            self.public_type.as_str(),
            arguments,
        )))
    }

    pub(crate) fn render_in(&self, value: &str) -> String {
        format!("({value}).UnsafeNative()")
    }

    pub(crate) fn render_out(&self, boundary_type: &GoType, value: &str) -> String {
        format!("{boundary_type}{{value: {value}}}")
    }

    fn render_declaration(&self, out: &mut String) {
        let declaration = go_type_parameter_declaration(self.arity);
        let arguments = go_type_parameter_use(self.arity);
        render_deprecation_comment(out, self.provenance.removed_at_version());
        writeln!(out, "type {}{declaration} struct {{", self.public_type)
            .expect("writing to String cannot fail");
        writeln!(out, "\t{} [0]struct{{}}", self.identity_field)
            .expect("writing to String cannot fail");
        writeln!(out, "\tvalue any\n}}\n").expect("writing to String cannot fail");
        render_deprecation_comment(out, self.provenance.removed_at_version());
        writeln!(
            out,
            "func {}{declaration}(value any) {}{arguments} {{",
            self.from_native, self.public_type
        )
        .expect("writing to String cannot fail");
        writeln!(
            out,
            "\treturn {}{arguments}{{value: value}}\n}}\n",
            self.public_type
        )
        .expect("writing to String cannot fail");
        render_deprecation_comment(out, self.provenance.removed_at_version());
        writeln!(
            out,
            "func (value {}{arguments}) UnsafeNative() any {{\n\treturn value.value\n}}\n",
            self.public_type
        )
        .expect("writing to String cannot fail");
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GoRetainedHostType {
    name: QualifiedTypeName,
    public_type: GoIdentifier,
    identity_field: GoIdentifier,
    removed_at_version: u32,
}

impl GoRetainedHostType {
    fn new(name: &QualifiedTypeName, removed_at_version: u32) -> Result<Self, GoFacadeError> {
        let frame = encode_qualified_type_frame(name);
        Ok(Self {
            name: name.clone(),
            public_type: go_identifier(format!("KioHost_{frame}"))?,
            identity_field: go_identifier(format!("kioDeprecatedHostTypeMarker_{frame}"))?,
            removed_at_version,
        })
    }

    pub(crate) fn name(&self) -> &QualifiedTypeName {
        &self.name
    }

    pub(crate) fn public_type(&self) -> &GoIdentifier {
        &self.public_type
    }

    fn render_declaration(&self, out: &mut String) {
        render_deprecation_comment(out, Some(self.removed_at_version));
        writeln!(out, "type {} struct {{", self.public_type)
            .expect("writing to String cannot fail");
        writeln!(out, "\t{} [0]struct{{}}", self.identity_field)
            .expect("writing to String cannot fail");
        writeln!(out, "}}\n").expect("writing to String cannot fail");
    }
}

impl GoRoleAdapter {
    pub(crate) fn name(&self) -> &QualifiedTypeName {
        &self.name
    }

    pub(crate) fn boundary_type(&self) -> &GoType {
        &self.boundary_type
    }

    pub(crate) fn internal_type(&self) -> &GoType {
        &self.internal_type
    }

    pub(crate) fn convert_in(&self) -> &GoIdentifier {
        &self.convert_in
    }

    pub(crate) fn convert_out(&self) -> &GoIdentifier {
        &self.convert_out
    }
}

impl GoHostBindingPlan {
    fn prepare<'a>(
        bindings: impl IntoIterator<Item = &'a BoundaryHostBinding>,
    ) -> Result<Self, GoFacadeError> {
        let mut plan = Self::default();
        for binding in bindings {
            let provenance = GoDeclarationProvenance::from_host_binding(binding.origin());
            if !binding.type_params().is_empty() {
                let carrier = GoHostTypeCarrier::new(
                    binding.name(),
                    binding.type_params().len(),
                    provenance,
                )?;
                if plan
                    .parametric_carriers
                    .insert(binding.name().clone(), carrier)
                    .is_some()
                {
                    return Err(GoFacadeError::new(
                        "one parametric Go host carrier occurred more than once",
                    ));
                }
                continue;
            }
            if let Some(removed_at_version) = provenance.removed_at_version() {
                // Go has no optional/default type argument. Per
                // `specs/backends/go.md` § Host-type removal cannot preserve
                // generic arity, history keeps a deprecated concrete nominal
                // instead of making this a current package-root selection.
                let retained = GoRetainedHostType::new(binding.name(), removed_at_version)?;
                if plan
                    .exact_types
                    .insert(
                        binding.name().clone(),
                        GoType::new(retained.public_type().as_str()),
                    )
                    .is_some()
                    || plan
                        .retained_types
                        .insert(binding.name().clone(), retained)
                        .is_some()
                {
                    return Err(GoFacadeError::new(
                        "one retained Go host type occurred more than once",
                    ));
                }
                continue;
            }
            let frame = encode_qualified_type_frame(binding.name());
            let name = go_identifier(format!("KioHost_{frame}"))?;
            let boundary_type = GoType::new(name.as_str());
            if plan
                .exact_types
                .insert(binding.name().clone(), boundary_type)
                .is_some()
            {
                return Err(GoFacadeError::new(
                    "one exact Go host binding occurred more than once",
                ));
            }
            plan.parameters.push(GoHostBindingParameter {
                name,
                constraint: ERASED_TYPE,
            });
            if let BoundaryHostTypeBinding::Role(role) = binding.binding() {
                let adapter_identity = readable_role_adapter_identity(
                    binding.name().module_segments(),
                    binding.name().name(),
                )
                .unwrap_or_else(|| frame.clone());
                let adapter = GoRoleAdapter {
                    name: binding.name().clone(),
                    boundary_type: plan.exact_types[binding.name()].clone(),
                    internal_type: GoType::new(go_role_type(role)),
                    convert_in: go_identifier(format!("KioHostIn_{adapter_identity}"))?,
                    convert_out: go_identifier(format!("KioHostOut_{adapter_identity}"))?,
                };
                if plan
                    .role_adapters
                    .insert(binding.name().clone(), adapter)
                    .is_some()
                {
                    return Err(GoFacadeError::new(
                        "one Go role adapter occurred more than once",
                    ));
                }
            }
        }
        Ok(plan)
    }

    fn exact_type(&self, name: &QualifiedTypeName) -> Option<&GoType> {
        self.exact_types.get(name)
    }

    fn role_adapter(&self, name: &QualifiedTypeName) -> Option<&GoRoleAdapter> {
        self.role_adapters.get(name)
    }

    fn role_adapters(&self) -> impl ExactSizeIterator<Item = &GoRoleAdapter> {
        self.role_adapters.values()
    }

    fn parametric_carrier(&self, name: &QualifiedTypeName) -> Option<&GoHostTypeCarrier> {
        self.parametric_carriers.get(name)
    }

    fn parametric_carriers(&self) -> impl ExactSizeIterator<Item = &GoHostTypeCarrier> {
        self.parametric_carriers.values()
    }

    fn retained_types(&self) -> impl ExactSizeIterator<Item = &GoRetainedHostType> {
        self.retained_types.values()
    }

    fn declaration(&self) -> String {
        if self.parameters.is_empty() {
            return String::new();
        }
        format!(
            "[{}]",
            self.parameters
                .iter()
                .map(|parameter| format!("{} {}", parameter.name, parameter.constraint))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn arguments(&self) -> String {
        if self.parameters.is_empty() {
            return String::new();
        }
        format!(
            "[{}]",
            self.parameters
                .iter()
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn encode_site_frame(site: &BoundaryFacadeSiteId) -> String {
    let mut encoded = format!(
        "V{SUM_USE_CODEC_VERSION}_M{}_",
        site.module_segments().len()
    );
    for segment in site.module_segments() {
        write_identity_component(&mut encoded, 'C', segment);
    }
    match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => {
            encoded.push_str("H_");
            write_identity_component(&mut encoded, 'N', name);
        }
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            encoded.push_str("E_");
            write_identity_component(&mut encoded, 'N', name);
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
            encoded.push_str("C_");
            write_identity_component(&mut encoded, 'T', newtype);
            write_identity_component(&mut encoded, 'N', member);
        }
        BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
            encoded.push_str("P_");
            write_identity_component(&mut encoded, 'T', newtype);
            write_identity_component(&mut encoded, 'N', member);
        }
    }
    encoded
}

fn write_identity_component(target: &mut String, tag: char, component: &str) {
    let component = crate::backends::public_names::host_name_core(component);
    let escaped = escape_identity_component(&component);
    write!(target, "{tag}{}_{}", escaped.len(), escaped).expect("writing to String cannot fail");
}

fn escape_identity_component(component: &str) -> String {
    let mut escaped = String::new();
    for byte in component.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => escaped.push(char::from(byte)),
            b'_' => escaped.push_str("_u"),
            _ => write!(&mut escaped, "_x{byte:02x}").expect("writing to String cannot fail"),
        }
    }
    escaped
}

#[derive(Clone, Debug)]
enum GoNominalPlan<'a> {
    Host {
        arity: usize,
        binding: BoundaryHostTypeBinding,
        exact_boundary_type: Option<GoType>,
        role_adapter: Option<GoRoleAdapter>,
        parametric_carrier: Option<GoHostTypeCarrier>,
    },
    Carrier {
        arity: usize,
        carrier: GoNewtypeCarrier,
    },
    Transparent {
        arity: usize,
        existential_count: usize,
        payload: &'a BoundaryNewtypePayloadPlan,
    },
}

impl GoNominalPlan<'_> {
    fn arity(&self) -> usize {
        match self {
            Self::Host { arity, .. }
            | Self::Carrier { arity, .. }
            | Self::Transparent { arity, .. } => *arity,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct GoReachability {
    shells: BTreeSet<FacadeShellId>,
    carriers: BTreeSet<GoNewtypeCarrier>,
    contextual_sums: BTreeMap<GoFacadeContext, GoType>,
}

impl GoReachability {
    fn merge(&mut self, other: Self) -> Result<(), GoFacadeError> {
        self.shells.extend(other.shells);
        self.carriers.extend(other.carriers);
        for (context, concrete_type) in other.contextual_sums {
            if let Some(previous) = self.contextual_sums.insert(context, concrete_type.clone())
                && previous != concrete_type
            {
                return Err(GoFacadeError::new(
                    "one semantic sum context rendered two concrete Row types",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct GoContextualSumRow {
    ordinal: u64,
    name: GoIdentifier,
    concrete_type: GoType,
    type_arguments: String,
}

#[derive(Clone, Debug)]
enum GoRenderResult {
    Complete(GoRenderedType),
    Cycle(QualifiedTypeName),
}

#[derive(Clone, Debug)]
struct GoRenderedType {
    ty: GoType,
    erasure: Option<GoErasureReason>,
}

impl GoRenderedType {
    fn concrete(ty: impl Into<String>) -> Self {
        Self {
            ty: GoType::new(ty),
            erasure: None,
        }
    }

    fn erased(reason: GoErasureReason) -> Self {
        Self {
            ty: GoType::new(ERASED_TYPE),
            erasure: Some(reason),
        }
    }
}

enum GoResolvedHead {
    Nominal {
        name: QualifiedTypeName,
        arguments: Vec<GoFacadeTraversal>,
        caller: GoFacadeTraversal,
    },
    Erased(GoErasureReason),
}

/// One prepared callable plus its exact site-local nominal snapshot.
///
/// The borrowed prepared site keeps semantic and live execution topology
/// paired by construction. A retained site exposes no execution value.
pub(crate) struct GoFacadeSite<'a> {
    prepared: PreparedBoundaryCallableSite<'a>,
    nominals: BTreeMap<QualifiedTypeName, GoNominalPlan<'a>>,
    contextual_sum_rows: BTreeMap<GoFacadeContext, GoContextualSumRow>,
}

impl<'a> GoFacadeSite<'a> {
    fn prepare(
        prepared: PreparedBoundaryCallableSite<'a>,
        root_bindings: &GoHostBindingPlan,
    ) -> Result<Self, GoFacadeError> {
        let mut nominals = BTreeMap::new();
        for (name, declaration) in prepared.nominals().declarations() {
            let plan = match declaration {
                BoundaryNominalDeclaration::HostType {
                    type_params,
                    binding,
                } => GoNominalPlan::Host {
                    arity: type_params.len(),
                    binding: *binding,
                    exact_boundary_type: root_bindings.exact_type(name).cloned(),
                    role_adapter: root_bindings.role_adapter(name).cloned(),
                    parametric_carrier: root_bindings.parametric_carrier(name).cloned(),
                },
                BoundaryNominalDeclaration::Newtype {
                    type_params,
                    existential_params,
                    transparent_payload: Some(payload),
                    surface: BoundaryNewtypeSurface::Both { .. },
                } => GoNominalPlan::Transparent {
                    arity: type_params.len(),
                    existential_count: existential_params.len(),
                    payload,
                },
                BoundaryNominalDeclaration::Newtype {
                    type_params,
                    transparent_payload: None,
                    surface,
                    ..
                } if surface.uses_nominal_carrier() => GoNominalPlan::Carrier {
                    arity: type_params.len(),
                    carrier: GoNewtypeCarrier::new(name)?,
                },
                BoundaryNominalDeclaration::Newtype { .. } => {
                    return Err(GoFacadeError::new(format!(
                        "prepared nominal {}.{} has inconsistent newtype surface/payload facts",
                        name.module_segments().join("."),
                        name.name()
                    )));
                }
            };
            if nominals.insert(name.clone(), plan).is_some() {
                return Err(GoFacadeError::new(
                    "one site-local nominal declaration occurred more than once",
                ));
            }
        }
        Ok(Self {
            prepared,
            nominals,
            contextual_sum_rows: BTreeMap::new(),
        })
    }

    pub(crate) fn id(&self) -> &BoundaryFacadeSiteId {
        self.prepared.site()
    }

    pub(crate) fn removed_at_version(&self) -> Option<u32> {
        self.prepared
            .retained()
            .map(|metadata| metadata.removed_at_version())
    }

    fn provenance(&self) -> GoDeclarationProvenance {
        self.removed_at_version()
            .map_or(GoDeclarationProvenance::Live, |removed_at_version| {
                GoDeclarationProvenance::Retained { removed_at_version }
            })
    }

    fn callable(&self) -> &'a BoundaryCallablePlan {
        self.prepared.plan()
    }

    fn execution(&self) -> Option<&'a CallableExecutionLayout> {
        self.prepared.execution()
    }

    pub(crate) fn live(&self) -> Option<GoLiveFacadeSite<'_, 'a>> {
        Some(GoLiveFacadeSite {
            site: self,
            execution: self.execution()?,
        })
    }

    fn root_facade(&self) -> GoFacadeArenaRef<'_, 'a> {
        GoFacadeArenaRef {
            site: self,
            arena: GoFacadeArenaId::Root,
            substitutions: Arc::new(BTreeMap::new()),
            bound_args: Arc::new(BTreeMap::new()),
            active_newtypes: Arc::new(Vec::new()),
        }
    }

    /// Exact semantic root used for stable live and retained FFI aliases.
    /// Runtime actions require the separate [`GoLiveFacadeSite`] capability.
    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn semantic_root(&self) -> Result<GoFacadeUseRef<'_, 'a>, GoFacadeError> {
        self.root_facade().root_use()
    }

    /// Semantic declaration cut used for stable live and retained aliases.
    ///
    /// The projection contextualizes every value slot and the post-cut return
    /// under the type binders consumed by earlier heads. It carries no live
    /// cursor, execution action, or ABI layout; those remain exclusive to
    /// [`GoLiveFacadeSite::callable_entry`].
    pub(crate) fn callable_entry(&self) -> Result<GoCallableEntry<'_, 'a>, GoFacadeError> {
        let semantic = self.callable().entry();
        let mut current = self.root_facade().root_use()?.traversal;
        let mut stages = Vec::with_capacity(semantic.head_stages.len());

        for semantic_stage in semantic.head_stages {
            match semantic_stage {
                BoundaryCallableHeadStage::Type { id, binder } => {
                    let FacadeUse::Forall {
                        binder: actual,
                        result,
                        ..
                    } = self.use_at(&current.context)?
                    else {
                        return Err(GoFacadeError::new(
                            "semantic type head does not point at a Forall use",
                        ));
                    };
                    if actual != &id {
                        return Err(GoFacadeError::new(
                            "semantic type head binder differs from its facade use",
                        ));
                    }
                    stages.push(GoCallableHeadStage::Type(GoCallableTypeHeadStage {
                        #[cfg(all(test, feature = "surface"))]
                        id,
                        binder,
                    }));

                    let GoFacadeTerm::Use {
                        source,
                        substitutions,
                    } = current.context.term.as_ref()
                    else {
                        unreachable!("a semantic Forall head has a semantic source")
                    };
                    let mut nested = substitutions.as_ref().clone();
                    nested.insert(id, GoFacadeContext::erased(GoErasureReason::UnboundType));
                    let mut bound_args = current.bound_args.as_ref().clone();
                    bound_args.insert(
                        id,
                        GoFacadeTraversal::erased(
                            GoErasureReason::UnboundType,
                            Arc::clone(&current.active_newtypes),
                        ),
                    );
                    current = GoFacadeTraversal {
                        context: GoFacadeContext::use_in(
                            source.arena.clone(),
                            *result,
                            Arc::new(nested),
                        ),
                        bound_args: Arc::new(bound_args),
                        active_newtypes: Arc::clone(&current.active_newtypes),
                    };
                }
                BoundaryCallableHeadStage::Value { slots } => {
                    let FacadeUse::Function {
                        slots: actual,
                        result,
                        ..
                    } = self.use_at(&current.context)?
                    else {
                        return Err(GoFacadeError::new(
                            "semantic value head does not point at a Function use",
                        ));
                    };
                    if actual.as_slice() != slots {
                        return Err(GoFacadeError::new(
                            "semantic value head slots differ from their facade use",
                        ));
                    }
                    let slots = slots
                        .iter()
                        .map(|slot| {
                            Ok(GoFacadeUseRef {
                                site: self,
                                traversal: self.child_traversal(&current, *slot)?,
                            })
                        })
                        .collect::<Result<Vec<_>, GoFacadeError>>()?;
                    stages.push(GoCallableHeadStage::Value(GoCallableValueHeadStage {
                        slots,
                    }));
                    current = self.child_traversal(&current, *result)?;
                }
            }
        }

        let Some(source) = Self::source_of(&current.context) else {
            return Err(GoFacadeError::new(
                "semantic callable returned cursor has no semantic source",
            ));
        };
        if source.use_id() != semantic.returned {
            return Err(GoFacadeError::new(
                "semantic callable traversal did not stop at its declared returned use",
            ));
        }
        Ok(GoCallableEntry {
            stages,
            returned: GoFacadeUseRef {
                site: self,
                traversal: current,
            },
        })
    }

    fn execution_use(&self, source: &GoFacadeUseSource) -> Option<&'a BoundaryFacadeExecutionUse> {
        let execution = self.execution()?;
        Some(match &source.arena {
            GoFacadeArenaId::Root => execution.root_uses().use_at(source.use_id),
            GoFacadeArenaId::TransparentPayload(name) => {
                execution.transparent_payload(name)?.use_at(source.use_id)
            }
        })
    }

    fn arena_plan(&self, arena: &GoFacadeArenaId) -> Result<&BoundaryFacadePlan, GoFacadeError> {
        match arena {
            GoFacadeArenaId::Root => Ok(self.callable().facade()),
            GoFacadeArenaId::TransparentPayload(name) => match self.nominals.get(name) {
                Some(GoNominalPlan::Transparent { payload, .. }) => Ok(payload.facade()),
                _ => Err(GoFacadeError::new(
                    "facade cursor names a missing transparent payload arena",
                )),
            },
        }
    }

    fn payload_plan(
        &self,
        name: &QualifiedTypeName,
    ) -> Result<(&GoNominalPlan<'a>, &'a BoundaryNewtypePayloadPlan), GoFacadeError> {
        let nominal = self.nominals.get(name).ok_or_else(|| {
            GoFacadeError::new(format!(
                "facade site is missing nominal {}.{}",
                name.module_segments().join("."),
                name.name()
            ))
        })?;
        match nominal {
            GoNominalPlan::Transparent { payload, .. } => Ok((nominal, payload)),
            _ => Err(GoFacadeError::new(
                "requested nominal does not have a transparent payload",
            )),
        }
    }

    fn instantiate_payload_arena(
        &self,
        name: &QualifiedTypeName,
        arguments: &[GoFacadeContext],
    ) -> Result<GoFacadeArenaRef<'_, 'a>, GoFacadeError> {
        let (nominal, payload) = self.payload_plan(name)?;
        let GoNominalPlan::Transparent {
            arity,
            existential_count,
            ..
        } = nominal
        else {
            unreachable!("payload_plan returns only transparent nominals")
        };
        if arguments.len() != *arity {
            return Err(GoFacadeError::new(format!(
                "transparent nominal {}.{} expected {} arguments, got {}",
                name.module_segments().join("."),
                name.name(),
                arity,
                arguments.len()
            )));
        }
        let declaration_binders = payload.declaration_binders();
        if declaration_binders.len() != arity + existential_count {
            return Err(GoFacadeError::new(
                "transparent payload binder inventory disagrees with its nominal declaration",
            ));
        }
        let mut substitutions = BTreeMap::new();
        for (binder, argument) in declaration_binders.iter().take(*arity).zip(arguments) {
            substitutions.insert(*binder, argument.clone());
        }
        for binder in declaration_binders.iter().skip(*arity) {
            substitutions.insert(
                *binder,
                GoFacadeContext::erased(GoErasureReason::Existential),
            );
        }
        Ok(GoFacadeArenaRef {
            site: self,
            arena: GoFacadeArenaId::TransparentPayload(name.clone()),
            substitutions: Arc::new(substitutions),
            bound_args: Arc::new(BTreeMap::new()),
            active_newtypes: Arc::new(Vec::new()),
        })
    }

    fn instantiate_payload_traversal(
        &self,
        name: &QualifiedTypeName,
        arguments: &[GoFacadeTraversal],
        caller: &GoFacadeTraversal,
    ) -> Result<GoFacadeTraversal, GoFacadeError> {
        let argument_contexts = arguments
            .iter()
            .map(|argument| argument.context.clone())
            .collect::<Vec<_>>();
        let arena = self.instantiate_payload_arena(name, &argument_contexts)?;
        let payload = arena.root_use()?;
        let (nominal, payload_plan) = self.payload_plan(name)?;
        let GoNominalPlan::Transparent {
            arity,
            existential_count,
            ..
        } = nominal
        else {
            unreachable!("payload_plan returns only transparent nominals")
        };
        let mut bound_args = BTreeMap::new();
        for (binder, argument) in payload_plan
            .declaration_binders()
            .iter()
            .take(*arity)
            .zip(arguments)
        {
            bound_args.insert(*binder, argument.clone());
        }
        for binder in payload_plan
            .declaration_binders()
            .iter()
            .skip(*arity)
            .take(*existential_count)
        {
            bound_args.insert(
                *binder,
                GoFacadeTraversal::erased(
                    GoErasureReason::Existential,
                    Arc::clone(&caller.active_newtypes),
                ),
            );
        }
        let mut active_newtypes = caller.active_newtypes.as_ref().clone();
        active_newtypes.push(name.clone());
        Ok(GoFacadeTraversal {
            context: payload.traversal.context,
            bound_args: Arc::new(bound_args),
            active_newtypes: Arc::new(active_newtypes),
        })
    }

    fn use_at(&self, context: &GoFacadeContext) -> Result<&FacadeUse, GoFacadeError> {
        let GoFacadeTerm::Use { source, .. } = context.term.as_ref() else {
            return Err(GoFacadeError::new(
                "an erased facade term has no source use",
            ));
        };
        let plan = self.arena_plan(&source.arena)?;
        plan.uses()
            .get(source.use_id.index())
            .ok_or_else(|| GoFacadeError::new("facade cursor use ID is outside its source arena"))
    }

    fn child_context(
        &self,
        parent: &GoFacadeContext,
        use_id: FacadeUseId,
    ) -> Result<GoFacadeContext, GoFacadeError> {
        let GoFacadeTerm::Use {
            source,
            substitutions,
        } = parent.term.as_ref()
        else {
            return Err(GoFacadeError::new("an erased facade term has no child use"));
        };
        let plan = self.arena_plan(&source.arena)?;
        if plan.uses().get(use_id.index()).is_none() {
            return Err(GoFacadeError::new(
                "facade child use ID is outside its source arena",
            ));
        }
        Ok(GoFacadeContext::use_in(
            source.arena.clone(),
            use_id,
            Arc::clone(substitutions),
        ))
    }

    fn child_traversal(
        &self,
        parent: &GoFacadeTraversal,
        use_id: FacadeUseId,
    ) -> Result<GoFacadeTraversal, GoFacadeError> {
        Ok(GoFacadeTraversal {
            context: self.child_context(&parent.context, use_id)?,
            bound_args: Arc::clone(&parent.bound_args),
            active_newtypes: Arc::clone(&parent.active_newtypes),
        })
    }

    fn resolve_bound_traversal(
        &self,
        traversal: &GoFacadeTraversal,
        binder: FacadeBinderId,
    ) -> Result<GoFacadeTraversal, GoFacadeError> {
        if let Some(argument) = traversal.bound_args.get(&binder) {
            return Ok(argument.clone());
        }
        let GoFacadeTerm::Use { substitutions, .. } = traversal.context.term.as_ref() else {
            return Err(GoFacadeError::new("an erased facade term has no binder"));
        };
        if substitutions.contains_key(&binder) {
            return Err(GoFacadeError::new(
                "a substituted facade binder is missing traversal provenance",
            ));
        }
        Ok(GoFacadeTraversal::erased(
            GoErasureReason::UnboundType,
            Arc::clone(&traversal.active_newtypes),
        ))
    }

    fn render_context(
        &self,
        traversal: &GoFacadeTraversal,
        reachability: &mut GoReachability,
    ) -> Result<GoRenderResult, GoFacadeError> {
        let context = &traversal.context;
        let GoFacadeTerm::Use { .. } = context.term.as_ref() else {
            let GoFacadeTerm::Erased(reason) = context.term.as_ref() else {
                unreachable!()
            };
            return Ok(GoRenderResult::Complete(GoRenderedType::erased(
                reason.clone(),
            )));
        };
        match self.use_at(context)? {
            FacadeUse::Unit { .. } => Ok(GoRenderResult::Complete(GoRenderedType::concrete(
                UNIT_TYPE,
            ))),
            FacadeUse::Bottom { .. } => Ok(GoRenderResult::Complete(GoRenderedType::erased(
                GoErasureReason::Bottom,
            ))),
            FacadeUse::Bound { binder, .. } => {
                let substituted = self.resolve_bound_traversal(traversal, *binder)?;
                self.render_context(&substituted, reachability)
            }
            FacadeUse::Nominal { name, .. } => {
                self.render_nominal(name, &[], traversal, reachability)
            }
            FacadeUse::Apply { .. } => {
                let head = self.resolve_application_head(traversal, Vec::new())?;
                match head {
                    GoResolvedHead::Nominal {
                        name,
                        arguments,
                        caller,
                    } => self.render_nominal(&name, &arguments, &caller, reachability),
                    GoResolvedHead::Erased(reason) => {
                        Ok(GoRenderResult::Complete(GoRenderedType::erased(reason)))
                    }
                }
            }
            FacadeUse::Product { shell, args, .. } => {
                reachability.shells.insert(shell.clone());
                let mut rendered = Vec::with_capacity(args.len());
                for argument in args {
                    let child = self.child_traversal(traversal, *argument)?;
                    match self.render_context(&child, reachability)? {
                        GoRenderResult::Complete(complete) => rendered.push(complete.ty),
                        GoRenderResult::Cycle(target) => {
                            return Ok(GoRenderResult::Cycle(target));
                        }
                    }
                }
                let shell = GoFacadeShell::new(shell)?;
                Ok(GoRenderResult::Complete(GoRenderedType {
                    ty: shell.application(&rendered)?,
                    erasure: None,
                }))
            }
            FacadeUse::Sum { shell, args, .. } => {
                reachability.shells.insert(shell.clone());
                let mut rendered = Vec::with_capacity(args.len());
                for argument in args {
                    let child = self.child_traversal(traversal, *argument)?;
                    match self.render_context(&child, reachability)? {
                        GoRenderResult::Complete(complete) => rendered.push(complete.ty),
                        GoRenderResult::Cycle(target) => {
                            return Ok(GoRenderResult::Cycle(target));
                        }
                    }
                }
                let shell = GoFacadeShell::new(shell)?;
                let concrete_row = shell.concrete_row_type(&rendered)?;
                if let Some(previous) = reachability
                    .contextual_sums
                    .insert(context.clone(), concrete_row.clone())
                    && previous != concrete_row
                {
                    return Err(GoFacadeError::new(
                        "one semantic sum context rendered two concrete Row types",
                    ));
                }
                Ok(GoRenderResult::Complete(GoRenderedType {
                    ty: shell.application(&rendered)?,
                    erasure: None,
                }))
            }
            FacadeUse::Function { slots, result, .. } => {
                let mut params = Vec::with_capacity(slots.len());
                for slot in slots {
                    let child = self.child_traversal(traversal, *slot)?;
                    match self.render_context(&child, reachability)? {
                        GoRenderResult::Complete(complete) => params.push(complete.ty),
                        GoRenderResult::Cycle(target) => {
                            return Ok(GoRenderResult::Cycle(target));
                        }
                    }
                }
                let result = self.child_traversal(traversal, *result)?;
                let result = match self.render_context(&result, reachability)? {
                    GoRenderResult::Complete(complete) => complete.ty,
                    GoRenderResult::Cycle(target) => return Ok(GoRenderResult::Cycle(target)),
                };
                Ok(GoRenderResult::Complete(GoRenderedType::concrete(format!(
                    "func({}) {result}",
                    params
                        .iter()
                        .map(GoType::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))))
            }
            FacadeUse::Forall { binder, result, .. } => {
                let GoFacadeTerm::Use {
                    source,
                    substitutions,
                } = context.term.as_ref()
                else {
                    unreachable!()
                };
                let mut nested = substitutions.as_ref().clone();
                let erased = GoFacadeContext::erased(GoErasureReason::UnboundType);
                nested.insert(*binder, erased.clone());
                let mut bound_args = traversal.bound_args.as_ref().clone();
                bound_args.insert(
                    *binder,
                    GoFacadeTraversal::erased(
                        GoErasureReason::UnboundType,
                        Arc::clone(&traversal.active_newtypes),
                    ),
                );
                let nested = GoFacadeTraversal {
                    context: GoFacadeContext::use_in(
                        source.arena.clone(),
                        *result,
                        Arc::new(nested),
                    ),
                    bound_args: Arc::new(bound_args),
                    active_newtypes: Arc::clone(&traversal.active_newtypes),
                };
                self.render_context(&nested, reachability)
            }
        }
    }

    fn render_nominal(
        &self,
        name: &QualifiedTypeName,
        arguments: &[GoFacadeTraversal],
        caller: &GoFacadeTraversal,
        reachability: &mut GoReachability,
    ) -> Result<GoRenderResult, GoFacadeError> {
        let nominal = self.nominals.get(name).ok_or_else(|| {
            GoFacadeError::new(format!(
                "facade site is missing nominal {}.{}",
                name.module_segments().join("."),
                name.name()
            ))
        })?;
        if arguments.len() != nominal.arity() {
            return Err(GoFacadeError::new(format!(
                "nominal {}.{} expected {} arguments, got {}",
                name.module_segments().join("."),
                name.name(),
                nominal.arity(),
                arguments.len()
            )));
        }
        match nominal {
            GoNominalPlan::Host {
                exact_boundary_type: Some(boundary_type),
                ..
            } => Ok(GoRenderResult::Complete(GoRenderedType::concrete(
                boundary_type.as_str(),
            ))),
            GoNominalPlan::Host {
                parametric_carrier: Some(carrier),
                ..
            } => {
                let mut rendered_arguments = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    match self.render_context(argument, reachability)? {
                        GoRenderResult::Complete(rendered) => {
                            rendered_arguments.push(rendered.ty);
                        }
                        GoRenderResult::Cycle(target) => return Ok(GoRenderResult::Cycle(target)),
                    }
                }
                Ok(GoRenderResult::Complete(GoRenderedType::concrete(
                    carrier.applied_type(&rendered_arguments)?.as_str(),
                )))
            }
            GoNominalPlan::Host { binding, .. } => match binding {
                BoundaryHostTypeBinding::Role(role) => Ok(GoRenderResult::Complete(
                    GoRenderedType::concrete(go_role_type(*role)),
                )),
                BoundaryHostTypeBinding::Roleless => Ok(GoRenderResult::Complete(
                    GoRenderedType::erased(GoErasureReason::RolelessHost(name.clone())),
                )),
            },
            GoNominalPlan::Carrier { carrier, .. } => {
                reachability.carriers.insert(carrier.clone());
                Ok(GoRenderResult::Complete(GoRenderedType::concrete(
                    carrier.public_type().as_str(),
                )))
            }
            GoNominalPlan::Transparent { .. } => {
                if caller.active_newtypes.contains(name) {
                    return Ok(GoRenderResult::Cycle(name.clone()));
                }
                let payload = self.instantiate_payload_traversal(name, arguments, caller)?;
                let mut local = GoReachability::default();
                match self.render_context(&payload, &mut local)? {
                    GoRenderResult::Complete(complete) => {
                        reachability.merge(local)?;
                        Ok(GoRenderResult::Complete(complete))
                    }
                    GoRenderResult::Cycle(target) if &target == name => {
                        Ok(GoRenderResult::Complete(GoRenderedType::erased(
                            GoErasureReason::RecursiveBoth(name.clone()),
                        )))
                    }
                    GoRenderResult::Cycle(target) => Ok(GoRenderResult::Cycle(target)),
                }
            }
        }
    }

    fn resolve_application_head(
        &self,
        traversal: &GoFacadeTraversal,
        trailing: Vec<GoFacadeTraversal>,
    ) -> Result<GoResolvedHead, GoFacadeError> {
        let context = &traversal.context;
        match context.term.as_ref() {
            GoFacadeTerm::Erased(reason) => Ok(GoResolvedHead::Erased(reason.clone())),
            GoFacadeTerm::Use { .. } => match self.use_at(context)? {
                FacadeUse::Apply {
                    constructor, args, ..
                } => {
                    let mut arguments = Vec::with_capacity(args.len() + trailing.len());
                    for argument in args {
                        arguments.push(self.child_traversal(traversal, *argument)?);
                    }
                    arguments.extend(trailing);
                    let constructor = self.child_traversal(traversal, *constructor)?;
                    self.resolve_application_head(&constructor, arguments)
                }
                FacadeUse::Bound { binder, .. } => {
                    let substituted = self.resolve_bound_traversal(traversal, *binder)?;
                    if matches!(substituted.context.term.as_ref(), GoFacadeTerm::Erased(_)) {
                        Ok(GoResolvedHead::Erased(
                            GoErasureReason::HigherKindedApplication,
                        ))
                    } else {
                        self.resolve_application_head(&substituted, trailing)
                    }
                }
                FacadeUse::Nominal { name, .. } => Ok(GoResolvedHead::Nominal {
                    name: name.clone(),
                    arguments: trailing,
                    caller: traversal.clone(),
                }),
                _ => Err(GoFacadeError::new(
                    "facade application has a non-constructor head",
                )),
            },
        }
    }

    fn collect_reachability(&self) -> Result<GoReachability, GoFacadeError> {
        let root = self.root_facade().root_use()?;
        let root = root.traversal;
        let mut reachability = GoReachability::default();
        match self.render_context(&root, &mut reachability)? {
            GoRenderResult::Complete(_) => Ok(reachability),
            GoRenderResult::Cycle(name) => Err(GoFacadeError::new(format!(
                "uncaught recursive facade expansion at {}.{}",
                name.module_segments().join("."),
                name.name()
            ))),
        }
    }

    fn install_contextual_sum_rows(
        &mut self,
        contexts: BTreeMap<GoFacadeContext, GoType>,
        type_arguments: &str,
    ) -> Result<(), GoFacadeError> {
        let site_frame = encode_site_frame(self.id());
        let mut rows = BTreeMap::new();
        for (ordinal, (context, concrete_type)) in contexts.into_iter().enumerate() {
            let ordinal = u64::try_from(ordinal).map_err(|_| {
                GoFacadeError::new("one site has more contextual sum uses than fit in u64")
            })?;
            let name = go_identifier(format!("kioSumUse_{site_frame}_U{ordinal}"))?;
            rows.insert(
                context,
                GoContextualSumRow {
                    ordinal,
                    name,
                    concrete_type,
                    type_arguments: type_arguments.to_owned(),
                },
            );
        }
        self.contextual_sum_rows = rows;
        Ok(())
    }
}

#[cfg(all(test, feature = "surface"))]
mod prepared_tests {
    use super::*;
    use crate::ast::Routed;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_package_file, parse_signature_file};
    use crate::pass::resolve::{Package, PackageFileEntry};
    use crate::pass::structural_recovery;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    const I32_HOST: &str = "KioHost_V1_M1_C3_apiN3_I32";

    fn package(source: &str) -> Package<Routed> {
        package_sources(&[("api", source)], "api;")
    }

    fn package_sources(sources: &[(&str, &str)], bridge: &str) -> Package<Routed> {
        let parsed = sources
            .iter()
            .map(|(name, source)| {
                (
                    PathBuf::from(format!("{name}.kio")),
                    parse(source).expect("parse facade test module"),
                )
            })
            .collect();
        let package_source = format!(
            "package pkg; build {{ target go {{ out \"out/go/\"; }} }} bridge {{ {bridge} }}"
        );
        let package_file =
            parse_package_file(&package_source, None).expect("parse facade test package file");
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, Some(package_file)).expect("lower package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file)
            .expect("build facade test package");
        package.resolve_imports().expect("resolve facade test uses");
        package
            .check_in_body_resolution()
            .expect("check facade test body resolution");
        let prime = check_package(&package).expect("typecheck facade test package");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    fn host_site(name: &str) -> BoundaryFacadeSiteId {
        BoundaryFacadeSiteId::new(
            vec!["api".to_owned()],
            BoundaryFacadeSiteOwner::HostFunction {
                name: name.to_owned(),
            },
        )
        .expect("valid host test site")
    }

    fn newtype_site(newtype: &str, member: &str, projector: bool) -> BoundaryFacadeSiteId {
        let owner = if projector {
            BoundaryFacadeSiteOwner::NewtypeProjector {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            }
        } else {
            BoundaryFacadeSiteOwner::NewtypeConstructor {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            }
        };
        BoundaryFacadeSiteId::new(vec!["api".to_owned()], owner).expect("valid newtype test site")
    }

    fn live_entry<'site, 'source>(
        catalog: &'site GoFacadeCatalog<'source>,
        name: &str,
    ) -> GoLiveCallableEntry<'site, 'source> {
        catalog
            .site(&host_site(name))
            .expect("prepared Go host site")
            .live()
            .expect("live Go host site")
            .callable_entry()
            .expect("paired Go callable entry")
    }

    fn first_value_slots<'entry, 'site, 'source>(
        entry: &'entry GoLiveCallableEntry<'site, 'source>,
    ) -> &'entry [GoLiveFacadeUseRef<'site, 'source>] {
        entry
            .stages()
            .iter()
            .find_map(|stage| stage.value_stage().map(GoLiveValueHeadStage::slots))
            .expect("one value head")
    }

    fn assert_erased_binder_context(cursor: &GoFacadeUseRef<'_, '_>, expected: &[FacadeBinderId]) {
        let expected = expected.iter().copied().collect::<BTreeSet<_>>();
        let GoFacadeTerm::Use { substitutions, .. } = cursor.traversal.context.term.as_ref() else {
            panic!("a callable-cut cursor retains its semantic source")
        };
        assert_eq!(
            substitutions.keys().copied().collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(
            cursor
                .traversal
                .bound_args
                .keys()
                .copied()
                .collect::<BTreeSet<_>>(),
            expected
        );
        for binder in expected {
            assert!(matches!(
                substitutions[&binder].term.as_ref(),
                GoFacadeTerm::Erased(GoErasureReason::UnboundType)
            ));
            assert!(matches!(
                cursor.traversal.bound_args[&binder].context.term.as_ref(),
                GoFacadeTerm::Erased(GoErasureReason::UnboundType)
            ));
        }
    }

    #[test]
    fn traversal_distinguishes_finite_nesting_from_demanded_cycles() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             pub newtype Id[A] : A { pub constructor make_id; pub projector read_id; }; \
             pub newtype Const[A] : . { pub constructor make_const; pub projector read_const; }; \
             pub rec newtype Self_cycle : Self_cycle { pub constructor make_self; pub projector read_self; }; \
             rec { \
               pub newtype Left : Right { pub constructor make_left; pub projector read_left; }; \
               pub newtype Right : Left { pub constructor make_right; pub projector read_right; }; \
             } \
             rec { \
               pub newtype Cut : Carrier { pub constructor make_cut; pub projector read_cut; }; \
               pub newtype Carrier : Cut { pub constructor make_carrier; projector read_carrier; }; \
             } \
             host fn nested(value: Id(Id(I32))) -> Id(Id(I32)); \
             host fn constant(value: Const(Self_cycle)) -> Const(Self_cycle); \
             host fn self_recursive(value: Self_cycle) -> Self_cycle; \
             host fn mutual_recursive(value: Left) -> Left; \
             host fn carrier_cut(value: Cut) -> Cut;",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare recursive facade sites");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize recursive Go facades");

        for cursor in first_value_slots(&live_entry(&catalog, "nested")) {
            assert_eq!(cursor.boundary_type().unwrap().as_str(), I32_HOST);
        }
        assert_eq!(
            live_entry(&catalog, "nested")
                .returned()
                .boundary_type()
                .unwrap()
                .as_str(),
            I32_HOST
        );
        assert_eq!(
            first_value_slots(&live_entry(&catalog, "constant"))[0]
                .boundary_type()
                .unwrap()
                .as_str(),
            "Unit"
        );
        for name in ["self_recursive", "mutual_recursive"] {
            assert_eq!(
                first_value_slots(&live_entry(&catalog, name))[0]
                    .boundary_type()
                    .unwrap()
                    .as_str(),
                "any"
            );
        }
        let carrier_type = first_value_slots(&live_entry(&catalog, "carrier_cut"))[0]
            .boundary_type()
            .unwrap();
        assert!(carrier_type.as_str().starts_with("KioNewtype_V1_"));
    }

    #[test]
    fn hkt_partial_head_restores_captured_provenance() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             pub newtype Id[A] : A { pub constructor make_id; pub projector read_id; }; \
             pub newtype N[*F][A] : F(A) { pub constructor make_n; pub projector read_n; }; \
             host fn hkt(value: N(N(Id), I32)) -> N(N(Id), I32);",
        );
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("prepare HKT facade site");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize HKT Go facade");
        let entry = live_entry(&catalog, "hkt");
        assert_eq!(
            first_value_slots(&entry)[0]
                .boundary_type()
                .unwrap()
                .as_str(),
            I32_HOST
        );
        assert_eq!(entry.returned().boundary_type().unwrap().as_str(), I32_HOST);
    }

    #[test]
    fn live_transparent_bound_uses_pair_actions_from_the_resolved_source() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             pub newtype Id[A] : A { pub constructor make_id; pub projector read_id; }; \
             host fn wrapped_function(value: Id(I32 -> I32)) -> .; \
             host fn wrapped_forall(value: Id([A] A -> A)) -> .;",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare transparent action facade sites");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize transparent action Go facades");

        let function_entry = live_entry(&catalog, "wrapped_function");
        let function_payload = match first_value_slots(&function_entry)[0].view().unwrap() {
            GoLiveFacadeUseView::Transparent { payload, .. } => payload,
            _ => panic!("Id(Function) must expose its transparent payload"),
        };
        let function_bound_source = function_payload.unresolved_source().cloned().unwrap();
        match function_payload.view().unwrap() {
            GoLiveFacadeUseView::Function {
                semantic, layout, ..
            } => {
                assert_ne!(&function_bound_source, semantic.source());
                assert_eq!(layout.facade_slot_count(), 1);
            }
            _ => panic!("Id(Function) must resolve its bound payload to Function"),
        }

        let forall_entry = live_entry(&catalog, "wrapped_forall");
        let forall_payload = match first_value_slots(&forall_entry)[0].view().unwrap() {
            GoLiveFacadeUseView::Transparent { payload, .. } => payload,
            _ => panic!("Id(Forall) must expose its transparent payload"),
        };
        let forall_bound_source = forall_payload.unresolved_source().cloned().unwrap();
        match forall_payload.view().unwrap() {
            GoLiveFacadeUseView::Forall { semantic, .. } => {
                assert_ne!(&forall_bound_source, semantic.source());
            }
            _ => panic!("Id(Forall) must resolve its bound payload to Forall"),
        }
    }

    #[test]
    fn ignored_transparent_argument_does_not_seed_root_shell_inventory() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             pub newtype Const[A] : . { pub constructor make_const; pub projector read_const; }; \
             host fn ignore(value: Const(I32 & I32)) -> Const(I32 & I32);",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare lazy facade site");
        assert!(
            prepared
                .shells()
                .any(|shell| shell.kind() == FacadeKind::Product)
        );
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize lazy Go facade");
        assert!(
            catalog
                .shells()
                .all(|shell| shell.id().kind() != FacadeKind::Product)
        );
        assert!(
            !catalog
                .render_declarations()
                .unwrap()
                .contains("type Product")
        );
    }

    #[test]
    fn contextual_sum_rows_are_site_local_and_instantiation_exact() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host type Str role(str); \
             host fn sums(value: ((I32 | Str) & (Str | I32))) -> .;",
        );
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("prepare contextual sums");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize contextual sums");
        let site = catalog.site(&host_site("sums")).expect("sum site");
        assert_eq!(site.contextual_sum_rows.len(), 2);
        let names = site
            .contextual_sum_rows
            .values()
            .map(|row| row.name.clone())
            .collect::<BTreeSet<_>>();
        let concrete = site
            .contextual_sum_rows
            .values()
            .map(|row| row.concrete_type.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 2);
        assert_eq!(concrete.len(), 2);

        let declarations = catalog.render_declarations().unwrap();
        for name in &names {
            assert!(
                declarations.contains(&format!("type {name}[")),
                "contextual rows that mention exact host roots must declare those roots as type parameters: {declarations}"
            );
        }
        let root = site.semantic_root().unwrap();
        let function = match root.view().unwrap() {
            GoFacadeUseView::Function(function) => function,
            _ => panic!("host function root must remain a Function facade"),
        };
        for slot in function.slots() {
            let sum = match slot.view().unwrap() {
                GoFacadeUseView::Sum(sum) => sum,
                _ => panic!("flattened host-function slots must retain contextual sums"),
            };
            assert!(
                sum.row_alias().as_str().contains('['),
                "contextual row uses must instantiate their exact host-root parameters"
            );
        }
    }

    #[test]
    fn paired_entry_consumes_interleaved_heads_once() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host fn staged[A](first: I32)[B](second: I32) -> I32;",
        );
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("prepare staged facade");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize staged facade");
        let live = catalog.site(&host_site("staged")).unwrap().live().unwrap();
        let entry = live.callable_entry().unwrap();
        assert_eq!(entry.stages().len(), 4);
        let first_type = entry.stages()[0].type_stage().unwrap();
        first_type.require_invoke_nullary();
        assert_eq!(first_type.binder().name, "A");
        assert!(entry.stages()[1].value_stage().is_some());
        let second_type = entry.stages()[2].type_stage().unwrap();
        second_type.require_invoke_nullary();
        assert_eq!(second_type.binder().name, "B");
        assert!(entry.stages()[3].value_stage().is_some());
        assert_eq!(entry.returned().boundary_type().unwrap().as_str(), I32_HOST);
    }

    #[test]
    fn ordinary_church_payload_does_not_mint_existential_projector_capability() {
        let package = package(
            "module api; \
             pub newtype Church : [R] ([Hidden] . -> R) -> R { \
               pub constructor make_church; pub projector read_church; \
             };",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare ordinary Church-shaped newtype sites");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize ordinary Church-shaped facade");
        let live = catalog
            .site(&newtype_site("Church", "read_church", true))
            .expect("ordinary Church-shaped projector site")
            .live()
            .expect("ordinary Church-shaped projector is live");
        let member = live
            .newtype_member_entry()
            .expect("validate ordinary Church-shaped projector");

        assert!(matches!(&member, GoLiveNewtypeMemberEntry::Projector(_)));
        assert_eq!(member.public_parameters().len(), 1);
        assert!(std::ptr::eq(member.result(), member.entry().returned()));
        assert!(matches!(
            member.entry().returned().view().unwrap(),
            GoLiveFacadeUseView::Forall { .. }
        ));
    }

    #[test]
    fn existential_member_capability_owns_identity_and_right_spine_cuts() {
        let package = package(
            "module api; \
             pub newtype Single <Hidden> : Hidden { \
               pub constructor make_single; pub projector read_single; \
             }; \
             pub newtype Packed[A] <Hidden> <Secret> : A & Hidden & Secret { \
               pub constructor make_packed; pub projector read_packed; \
             };",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare existential newtype sites");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize existential newtype facades");

        let single = catalog
            .site(&newtype_site("Single", "read_single", true))
            .expect("identity existential projector site")
            .live()
            .expect("identity existential projector is live")
            .newtype_member_entry()
            .expect("validate identity existential projector");
        let GoLiveNewtypeMemberEntry::ExistentialProjector(single_projector) = &single else {
            panic!("identity existential projector must have its nominal-derived class");
        };
        let single_parameters = single.public_parameters();
        assert_eq!(single_parameters.len(), 2);
        assert!(std::ptr::eq(
            single_parameters[1],
            single_projector.continuation()
        ));
        assert!(std::ptr::eq(
            single.result(),
            single_projector.selected_result()
        ));
        let single_payload_function = match single_projector.continuation().view().unwrap() {
            GoLiveFacadeUseView::Forall { semantic, result } => {
                assert_eq!(semantic.binder_metadata().name, "Hidden");
                result
            }
            _ => panic!("identity continuation must retain its existential Forall"),
        };
        match single_payload_function.view().unwrap() {
            GoLiveFacadeUseView::Function {
                layout,
                slots,
                result,
                ..
            } => {
                assert_eq!(slots.len(), 1);
                assert_eq!(layout.body_abi_arity(), 1);
                assert_eq!(layout.source_params().len(), 1);
                assert_eq!(layout.source_params()[0].facade_slots(), 0..1);
                assert_eq!(
                    layout.source_params()[0].adapter(),
                    CallableSourceParamAdapter::Identity
                );
                assert!(matches!(
                    result.view().unwrap(),
                    GoLiveFacadeUseView::Erased {
                        reason: GoErasureReason::UnboundType,
                        ..
                    }
                ));
            }
            _ => panic!("identity continuation must end in its payload Function"),
        }

        let packed = catalog
            .site(&newtype_site("Packed", "read_packed", true))
            .expect("right-spine existential projector site")
            .live()
            .expect("right-spine existential projector is live")
            .newtype_member_entry()
            .expect("validate right-spine existential projector");
        let GoLiveNewtypeMemberEntry::ExistentialProjector(packed_projector) = &packed else {
            panic!("right-spine existential projector must have its nominal-derived class");
        };
        assert_eq!(packed.entry().stages().len(), 2);
        assert_eq!(
            packed.entry().stages()[0]
                .type_stage()
                .expect("generic projector universal head")
                .binder()
                .name,
            "A"
        );
        assert_eq!(packed.public_parameters().len(), 2);

        let mut packed_payload_function = packed_projector.continuation().clone();
        for expected in ["Hidden", "Secret"] {
            packed_payload_function = match packed_payload_function.view().unwrap() {
                GoLiveFacadeUseView::Forall { semantic, result } => {
                    assert_eq!(semantic.binder_metadata().name, expected);
                    result
                }
                _ => panic!("right-spine continuation lost existential Forall {expected}"),
            };
        }
        match packed_payload_function.view().unwrap() {
            GoLiveFacadeUseView::Function {
                layout,
                slots,
                result,
                ..
            } => {
                assert_eq!(slots.len(), 3);
                assert!(
                    slots
                        .iter()
                        .all(|slot| slot.boundary_type().unwrap().as_str() == "any")
                );
                assert_eq!(layout.body_abi_arity(), 1);
                assert_eq!(layout.source_params().len(), 1);
                assert_eq!(layout.source_params()[0].facade_slots(), 0..3);
                assert_eq!(
                    layout.source_params()[0].adapter(),
                    CallableSourceParamAdapter::RightNest
                );
                assert!(matches!(
                    result.view().unwrap(),
                    GoLiveFacadeUseView::Erased {
                        reason: GoErasureReason::UnboundType,
                        ..
                    }
                ));
            }
            _ => panic!("right-spine continuation must end in its payload Function"),
        }

        let constructor = catalog
            .site(&newtype_site("Packed", "make_packed", false))
            .expect("existential constructor site")
            .live()
            .expect("existential constructor is live")
            .newtype_member_entry()
            .expect("validate existential constructor");
        assert!(matches!(
            &constructor,
            GoLiveNewtypeMemberEntry::Constructor(_)
        ));
        assert_eq!(constructor.entry().stages().len(), 4);
        let type_head_names = constructor.entry().stages()[..3]
            .iter()
            .map(|stage| {
                stage
                    .type_stage()
                    .expect("constructor owner type head")
                    .binder()
                    .name
                    .as_str()
            })
            .collect::<Vec<_>>();
        assert_eq!(type_head_names, ["A", "Hidden", "Secret"]);
        assert_eq!(constructor.public_parameters().len(), 3);
    }

    #[test]
    fn existential_projector_accepts_a_canonical_nullary_unit_payload() {
        let package = package(
            "module api; \
             pub type Erased[A] = .; \
             pub newtype Empty <Hidden> : Erased(Hidden) { \
               pub constructor make_empty; pub projector read_empty; \
             };",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare nullary existential projector");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize nullary existential projector");
        let member = catalog
            .site(&newtype_site("Empty", "read_empty", true))
            .expect("nullary existential projector site")
            .live()
            .expect("nullary existential projector is live")
            .newtype_member_entry()
            .expect("validate nullary existential projector");
        let GoLiveNewtypeMemberEntry::ExistentialProjector(projector) = member else {
            panic!("nullary existential projector must retain its existential capability");
        };
        let payload_function = match projector.continuation().view().unwrap() {
            GoLiveFacadeUseView::Forall { result, .. } => result,
            _ => panic!("nullary existential continuation must retain its binder"),
        };
        match payload_function.view().unwrap() {
            GoLiveFacadeUseView::Function { layout, slots, .. } => {
                assert!(slots.is_empty());
                assert_eq!(layout.source_param_count(), 0);
                assert_eq!(layout.body_abi_arity(), 0);
                assert!(layout.source_params().is_empty());
            }
            _ => panic!("nullary existential continuation must end in a Function"),
        }
    }

    #[test]
    fn canonical_unit_source_is_nullary_after_recovery() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host fn abi_zero() -> .; \
             host fn explicit_unit(value: .) -> .; \
             host fn grouped(first: I32, rest: I32 & I32) -> .;",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare zero-argument facade sites");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize zero-argument Go facades");

        fn value_capability<'entry, 'site, 'source>(
            entry: &'entry GoLiveCallableEntry<'site, 'source>,
        ) -> &'entry GoLiveValueHeadStage<'site, 'source> {
            entry
                .stages()
                .iter()
                .find_map(GoLiveHeadStage::value_stage)
                .expect("one opaque live value-head capability")
        }

        let zero_entry = live_entry(&catalog, "abi_zero");
        let zero = value_capability(&zero_entry);
        assert!(zero.slots().is_empty());
        assert!(zero.source_groups().unwrap().is_empty());

        let unit_entry = live_entry(&catalog, "explicit_unit");
        let unit = value_capability(&unit_entry);
        assert!(unit.slots().is_empty());
        assert!(unit.source_groups().unwrap().is_empty());

        let grouped_entry = live_entry(&catalog, "grouped");
        let grouped = value_capability(&grouped_entry);
        assert_eq!(grouped.slots().len(), 3);
        let grouped_sources = grouped.source_groups().unwrap();
        assert_eq!(grouped_sources.len(), 2);
        match grouped_sources[0].view() {
            GoLiveValueSourceGroupView::Identity(slot) => {
                assert!(std::ptr::eq(slot, &grouped.slots()[0]));
            }
            _ => panic!("the first source parameter must bind its identity slot"),
        }
        match grouped_sources[1].view() {
            GoLiveValueSourceGroupView::RightNest(slots) => {
                assert_eq!(slots.len(), 2);
                assert!(std::ptr::eq(slots.as_ptr(), grouped.slots()[1..].as_ptr()));
            }
            _ => panic!("the final product parameter must bind its right-nested slots"),
        }
    }

    #[test]
    fn retained_semantic_entry_contextualizes_interleaved_heads_without_execution() {
        let package = package("module api;");
        let signature = parse_signature_file(
            r#"signature pkg v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old[A](first: A)[B](second: B & A) -> A & B;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
            None,
        )
        .expect("parse retained semantic-entry signature");
        let replayed =
            crate::sig::replay(&signature).expect("replay retained semantic-entry signature");
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("prepare retained semantic-entry site");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize retained semantic-entry facade");
        let site = catalog.site(&host_site("old")).expect("retained old host");

        assert!(site.live().is_none());
        assert!(
            site.live()
                .map(|live| live.newtype_member_entry())
                .is_none()
        );
        assert_eq!(catalog.live_sites().count(), 0);
        let entry = site
            .callable_entry()
            .expect("contextualize retained semantic callable entry");
        assert_eq!(entry.stages().len(), 4);

        let first_type = entry.stages()[0]
            .type_stage()
            .expect("first retained head is type-valued");
        assert_eq!(first_type.binder().name, "A");
        let first_value = entry.stages()[1]
            .value_stage()
            .expect("second retained head is value-valued");
        assert_eq!(first_value.slots().len(), 1);
        assert_eq!(
            first_value.slots()[0].boundary_type().unwrap().as_str(),
            "any"
        );
        assert_erased_binder_context(first_value.slots().first().unwrap(), &[first_type.id()]);

        let second_type = entry.stages()[2]
            .type_stage()
            .expect("third retained head is type-valued");
        assert_eq!(second_type.binder().name, "B");
        assert_ne!(first_type.id(), second_type.id());
        let second_value = entry.stages()[3]
            .value_stage()
            .expect("fourth retained head is value-valued");
        assert_eq!(second_value.slots().len(), 2);
        assert!(
            second_value
                .slots()
                .iter()
                .all(|slot| slot.boundary_type().unwrap().as_str() == "any")
        );
        for slot in second_value.slots() {
            assert_erased_binder_context(slot, &[first_type.id(), second_type.id()]);
        }

        let returned_type = entry.returned().boundary_type().unwrap();
        assert!(returned_type.as_str().ends_with("[any, any]"));
        assert_erased_binder_context(entry.returned(), &[first_type.id(), second_type.id()]);
        match entry.returned().view().unwrap() {
            GoFacadeUseView::Product(product) => {
                assert_eq!(product.fields().len(), 2);
                assert!(
                    product.fields().iter().all(|field| {
                        field.payload().boundary_type().unwrap().as_str() == "any"
                    })
                );
            }
            _ => panic!("retained callable return must remain its contextual Product"),
        }
    }

    #[test]
    fn retained_compound_and_carrier_emit_declarations_but_no_live_site() {
        let package = package("module api;");
        let signature = parse_signature_file(
            r#"signature pkg v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Carrier : . { pub constructor make; projector read; };
        host fn old(value: (. | (. & Carrier))) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Carrier;
        old;
      }
    }
  }
}
"#,
            None,
        )
        .expect("parse retained facade signature");
        let replayed = crate::sig::replay(&signature).expect("replay retained facade signature");
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("prepare retained facade site");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize retained Go facade");

        assert_eq!(catalog.live_sites().count(), 0);
        let old = catalog.site(&host_site("old")).expect("retained old host");
        assert!(old.live().is_none());
        assert!(old.semantic_root().is_ok());
        let product_name = catalog
            .shells()
            .find(|shell| shell.id().kind() == FacadeKind::Product)
            .expect("retained nested product shell")
            .name()
            .to_string();
        let sum_name = catalog
            .shells()
            .find(|shell| shell.id().kind() == FacadeKind::Sum)
            .expect("retained root sum shell")
            .name()
            .to_string();
        assert_eq!(catalog.carriers().count(), 1);
        let declarations = catalog.render_declarations().unwrap();
        assert!(declarations.contains(&format!("type {product_name}[")));
        assert!(declarations.contains(&format!("type {sum_name}_Row[")));
        assert!(declarations.contains("type KioNewtype_V1_"));
    }

    #[test]
    fn bridged_opaque_inventory_emits_carrier_without_callable_sites() {
        let package = package(
            "module api; \
             pub newtype Opaque : . { constructor make_opaque; projector read_opaque; };",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare opaque-only inventory");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize opaque-only inventory");
        assert_eq!(catalog.sites().count(), 0);
        assert_eq!(catalog.carriers().count(), 1);
        let declarations = catalog.render_declarations().unwrap();
        assert_eq!(declarations.matches("type KioNewtype_V1_").count(), 1);
        assert!(declarations.contains("struct {"));
    }

    #[test]
    fn unrelated_same_leaf_inventory_only_adds_its_identity_derived_carrier() {
        let api = "module api; \
            host type I32 role(i32); \
            pub newtype Token : . { constructor make_token; projector read_token; }; \
            host fn stable(value: (I32 | Token)) -> (I32 | Token);";
        let base_package = package(api);
        let base_prepared = PreparedBoundaryCallableSites::collect_live(&base_package)
            .expect("prepare base open-world facade");
        let base =
            GoFacadeCatalog::prepare(&base_prepared).expect("realize base open-world facade");

        let extended_package = package_sources(
            &[
                ("api", api),
                (
                    "other",
                    "module other; \
                     pub newtype Token : . { constructor make_token; projector read_token; };",
                ),
            ],
            "api; other;",
        );
        let extended_prepared = PreparedBoundaryCallableSites::collect_live(&extended_package)
            .expect("prepare extended open-world facade");
        let extended = GoFacadeCatalog::prepare(&extended_prepared)
            .expect("realize extended open-world facade");

        let base_site = base.site(&host_site("stable")).unwrap();
        let extended_site = extended.site(&host_site("stable")).unwrap();
        assert_eq!(
            base_site.semantic_root().unwrap().boundary_type().unwrap(),
            extended_site
                .semantic_root()
                .unwrap()
                .boundary_type()
                .unwrap()
        );
        assert_eq!(
            base_site
                .contextual_sum_rows
                .values()
                .map(|row| (&row.name, &row.concrete_type))
                .collect::<Vec<_>>(),
            extended_site
                .contextual_sum_rows
                .values()
                .map(|row| (&row.name, &row.concrete_type))
                .collect::<Vec<_>>()
        );
        for (scope, name, owner) in base.claims().iter() {
            assert_eq!(extended.claims().owner(scope, name), Some(owner));
        }
        assert_eq!(base.carriers().count(), 1);
        assert_eq!(extended.carriers().count(), 2);
        assert!(
            extended.render_declarations().unwrap().len()
                > base.render_declarations().unwrap().len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qualified(module: &[&str], name: &str) -> QualifiedTypeName {
        QualifiedTypeName::new(
            module.iter().map(|segment| (*segment).to_owned()).collect(),
            name,
        )
        .expect("nonempty qualified test identity")
    }

    fn positional_shell(kind: FacadeKind, arity: u64) -> FacadeShellId {
        FacadeShellId::new(
            kind,
            (0..arity)
                .map(|index| SemanticKey::Positional { index })
                .collect(),
        )
    }

    #[test]
    fn carrier_codec_preserves_complete_qname_and_word_boundaries() {
        let flat = GoNewtypeCarrier::new(&qualified(&["a_b"], "Token")).unwrap();
        let nested = GoNewtypeCarrier::new(&qualified(&["a", "b"], "Token")).unwrap();
        let underscore = GoNewtypeCarrier::new(&qualified(&["a"], "T_oken")).unwrap();
        let affixed = GoNewtypeCarrier::new(&qualified(&["a"], "_Token_")).unwrap();

        assert_ne!(flat.public_type(), nested.public_type());
        assert_ne!(flat.public_type(), underscore.public_type());
        assert_ne!(underscore.public_type(), affixed.public_type());
        assert!(flat.public_type().as_str().starts_with("KioNewtype_V1_"));
        assert!(underscore.public_type().as_str().contains("N5_TOken"));
        assert!(affixed.public_type().as_str().contains("N9__uToken_u"));

        let same = GoNewtypeCarrier::new(&qualified(&["a_b"], "Token")).unwrap();
        assert_eq!(flat, same);
    }

    #[test]
    fn carriers_are_concrete_arity_independent_and_conversion_distinct() {
        let left = GoNewtypeCarrier::new(&qualified(&["left"], "Token")).unwrap();
        let right = GoNewtypeCarrier::new(&qualified(&["right"], "Token")).unwrap();
        let mut declarations = String::new();
        left.render_declaration(&mut declarations);
        right.render_declaration(&mut declarations);

        assert!(declarations.contains(&format!("type {} struct {{", left.public_type())));
        assert!(!declarations.contains(&format!("type {} interface", left.public_type())));
        assert!(declarations.contains("[0]struct{}"));
        assert_ne!(left.identity_field, right.identity_field);
        assert_ne!(left.value_accessor(), right.value_accessor());
        assert_eq!(left.storage_type(), left.public_type());
        assert_eq!(
            left.render_out("raw"),
            format!("{}{{value: raw}}", left.public_type())
        );
        assert_eq!(
            left.render_in("wrapped"),
            format!("(wrapped).{}()", left.value_accessor())
        );

        // The declaration has no type parameters: changing a source epoch's
        // declaration arity cannot create a second carrier ABI.
        assert!(
            !declarations
                .lines()
                .find(|line| line.starts_with(&format!("type {} ", left.public_type())))
                .unwrap()
                .contains('[')
        );
    }

    #[test]
    fn sum_support_has_valid_zero_and_row_dependent_storage() {
        let mut declaration = String::new();
        render_sum_support(&mut declaration);

        assert!(declaration.contains("zeroCase(R) kioCaseMarker"));
        assert!(declaration.contains("phantom [0]*R"));
        assert!(declaration.contains("if value.stored != nil"));
        assert!(declaration.contains("return row.zeroCase(row)"));
        assert!(declaration.contains("func (value KioSum[R]) Case() kioCaseMarker"));
    }

    #[test]
    fn sum_shell_zero_is_first_arm_and_helpers_are_row_anchored() {
        let shell = GoFacadeShell::new(&positional_shell(FacadeKind::Sum, 2)).unwrap();
        let mut shell_declaration = String::new();
        shell.render_declaration(&mut shell_declaration).unwrap();
        assert!(shell_declaration.contains("zeroCase(_ Sum_Row[T0, T1])"));
        assert!(shell_declaration.contains("kioSum_K0_CaseValue[Sum_Row[T0, T1], T0]{}"));

        let key = GoSumKey::new(&SemanticKey::Positional { index: 0 }).unwrap();
        let mut key_declaration = String::new();
        key.render_declaration(&mut key_declaration);
        assert!(key_declaration.contains("kioSum_K0_CaseMarker(R)"));
        assert!(
            key_declaration
                .contains("func NewKioSum_K0[R kioSum_K0_Row[R, P], P any](value P) KioSum[R]")
        );
    }

    #[test]
    fn equal_arm_keys_share_linear_helpers_but_rows_keep_cases_separate() {
        let alpha = GoFacadeShell::new(&FacadeShellId::new(
            FacadeKind::Sum,
            vec![
                SemanticKey::Bare {
                    name: "Shared".to_owned(),
                },
                SemanticKey::Bare {
                    name: "Alpha".to_owned(),
                },
            ],
        ))
        .unwrap();
        let beta = GoFacadeShell::new(&FacadeShellId::new(
            FacadeKind::Sum,
            vec![
                SemanticKey::Bare {
                    name: "Shared".to_owned(),
                },
                SemanticKey::Bare {
                    name: "Beta".to_owned(),
                },
            ],
        ))
        .unwrap();
        assert_eq!(alpha.sum_keys[0], beta.sum_keys[0]);
        assert_ne!(alpha.row_name(), beta.row_name());

        let helper = &alpha.sum_keys[0];
        let alpha_case = apply_go_type(
            helper.public_case.as_str(),
            &[
                GoType::new(alpha.row_name().unwrap().as_str()),
                GoType::new("P"),
            ],
        );
        let beta_case = apply_go_type(
            helper.public_case.as_str(),
            &[
                GoType::new(beta.row_name().unwrap().as_str()),
                GoType::new("P"),
            ],
        );
        assert_ne!(alpha_case, beta_case);
    }

    #[test]
    fn wide_sum_source_is_linear_in_arm_count() {
        fn render(arity: u64) -> (GoFacadeShell, String) {
            let shell = GoFacadeShell::new(&positional_shell(FacadeKind::Sum, arity)).unwrap();
            let mut declaration = String::new();
            for helper in shell
                .sum_keys
                .iter()
                .map(|helper| (helper.key.clone(), helper.clone()))
                .collect::<BTreeMap<_, _>>()
                .values()
            {
                helper.render_declaration(&mut declaration);
            }
            shell.render_declaration(&mut declaration).unwrap();
            (shell, declaration)
        }

        let (_, reference) = render(257);
        let (shell, declaration) = render(513);
        let row = shell.row_name().unwrap();

        assert_eq!(declaration.matches("func NewKioSum_").count(), 513);
        assert!(declaration.matches(shell.name().as_str()).count() <= 8);
        assert!(declaration.matches(row.as_str()).count() <= 16);
        assert!(declaration.len() > reference.len());
        assert!(declaration.len() <= reference.len() * 3);
        assert!(declaration.len() < 1_000_000);
    }

    #[test]
    fn ordinary_product_and_sum_claims_match_rendered_declarations() {
        let product = GoFacadeShell::new(&positional_shell(FacadeKind::Product, 2)).unwrap();
        let sum = GoFacadeShell::new(&positional_shell(FacadeKind::Sum, 2)).unwrap();
        let shells = BTreeMap::from([
            (product.id().clone(), product.clone()),
            (sum.id().clone(), sum.clone()),
        ]);
        let sum_keys = sum
            .sum_keys
            .iter()
            .map(|helper| (helper.key.clone(), helper.clone()))
            .collect::<BTreeMap<_, _>>();
        let claims =
            build_facade_claims(&BTreeMap::new(), &shells, &sum_keys, &BTreeMap::new()).unwrap();

        for name in ["Unit", "Product", "Sum", "KioSum"] {
            assert!(
                claims
                    .owner(
                        &GoFacadeScope::Package,
                        &go_identifier(name.to_owned()).unwrap(),
                    )
                    .is_some()
            );
        }
        assert!(sum.row_name().unwrap().as_str().contains('_'));
        assert!(sum_keys.values().all(|helper| {
            helper.public_case.as_str().contains('_') && helper.constructor.as_str().contains('_')
        }));
    }

    #[test]
    fn unreachable_product_and_sum_are_not_phantom_claims() {
        let claims = build_facade_claims(
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        for name in ["Product", "Sum"] {
            assert!(
                claims
                    .owner(
                        &GoFacadeScope::Package,
                        &go_identifier(name.to_owned()).unwrap(),
                    )
                    .is_none()
            );
        }
        for name in ["Unit", "KioSum"] {
            assert!(
                claims
                    .owner(
                        &GoFacadeScope::Package,
                        &go_identifier(name.to_owned()).unwrap(),
                    )
                    .is_some()
            );
        }
    }

    #[test]
    fn exact_alias_owner_rejects_same_spelling_for_another_sum_context() {
        let alias = go_identifier("Env_Api_value_ret".to_owned()).unwrap();
        let row_a = go_identifier("kioSumUse_V1_M1_C3_apiH_N1_a_U0".to_owned()).unwrap();
        let row_b = go_identifier("kioSumUse_V1_M1_C3_apiH_N1_b_U0".to_owned()).unwrap();
        let collision = ScopeClaims::try_collect([
            (
                GoFacadeScope::Package,
                alias.clone(),
                GoSumAliasNameOwner::Alias {
                    row: row_a,
                    alias: alias.clone(),
                },
            ),
            (
                GoFacadeScope::Package,
                alias.clone(),
                GoSumAliasNameOwner::Alias { row: row_b, alias },
            ),
        ]);
        assert!(collision.is_err());
    }

    #[test]
    fn contextual_row_site_frame_preserves_module_and_owner_boundaries() {
        let host = |module: &[&str], name: &str| {
            BoundaryFacadeSiteId::new(
                module.iter().map(|segment| (*segment).to_owned()).collect(),
                BoundaryFacadeSiteOwner::HostFunction {
                    name: name.to_owned(),
                },
            )
            .unwrap()
        };
        let export = BoundaryFacadeSiteId::new(
            vec!["a_b".to_owned()],
            BoundaryFacadeSiteOwner::ExportedFunction {
                name: "call".to_owned(),
            },
        )
        .unwrap();
        assert_ne!(
            encode_site_frame(&host(&["a_b"], "call")),
            encode_site_frame(&host(&["a", "b"], "call"))
        );
        assert_ne!(
            encode_site_frame(&host(&["a_b"], "call")),
            encode_site_frame(&export)
        );
    }

    #[test]
    fn recursion_ancestry_is_not_part_of_context_identity() {
        let context = GoFacadeContext::erased(GoErasureReason::UnboundType);
        let first_walk = vec![qualified(&["api"], "Outer")];
        let second_walk = vec![qualified(&["api"], "Other"), qualified(&["api"], "Outer")];
        assert_ne!(first_walk, second_walk);
        assert_eq!(context, context.clone());
        assert_eq!(BTreeSet::from([context]).len(), 1);
    }
}

/// Semantic declaration-owned heads and the exact cursor after their cut.
///
/// This projection is available for both live and retained sites. Its cursors
/// support type/name realization only; no execution fact is present.
pub(crate) struct GoCallableEntry<'site, 'source> {
    stages: Vec<GoCallableHeadStage<'site, 'source>>,
    returned: GoFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> GoCallableEntry<'site, 'source> {
    pub(crate) fn stages(&self) -> &[GoCallableHeadStage<'site, 'source>] {
        &self.stages
    }

    pub(crate) fn returned(&self) -> &GoFacadeUseRef<'site, 'source> {
        &self.returned
    }
}

/// One semantic declaration-owned head with no execution capability.
pub(crate) enum GoCallableHeadStage<'site, 'source> {
    Type(GoCallableTypeHeadStage<'source>),
    Value(GoCallableValueHeadStage<'site, 'source>),
}

impl<'site, 'source> GoCallableHeadStage<'site, 'source> {
    pub(crate) fn type_stage(&self) -> Option<&GoCallableTypeHeadStage<'source>> {
        match self {
            Self::Type(stage) => Some(stage),
            Self::Value(_) => None,
        }
    }

    pub(crate) fn value_stage(&self) -> Option<&GoCallableValueHeadStage<'site, 'source>> {
        match self {
            Self::Value(stage) => Some(stage),
            Self::Type(_) => None,
        }
    }
}

/// Semantic metadata for one declaration-owned type head.
pub(crate) struct GoCallableTypeHeadStage<'source> {
    #[cfg(all(test, feature = "surface"))]
    id: FacadeBinderId,
    binder: &'source FacadeBinder,
}

impl<'source> GoCallableTypeHeadStage<'source> {
    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn id(&self) -> FacadeBinderId {
        self.id
    }

    pub(crate) fn binder(&self) -> &'source FacadeBinder {
        self.binder
    }
}

/// Contextual facade slots for one declaration-owned value head.
pub(crate) struct GoCallableValueHeadStage<'site, 'source> {
    slots: Vec<GoFacadeUseRef<'site, 'source>>,
}

impl<'site, 'source> GoCallableValueHeadStage<'site, 'source> {
    pub(crate) fn slots(&self) -> &[GoFacadeUseRef<'site, 'source>] {
        &self.slots
    }
}

/// Exact live classification of one public newtype member.
///
/// The opaque payloads can be minted only after the member identity, its
/// site-local nominal declaration, every declaration head, and the relevant
/// live post-cut topology agree. In particular, a projector whose ordinary
/// payload happens to have a Church/CPS shape cannot manufacture the
/// existential variant.
pub(crate) enum GoLiveNewtypeMemberEntry<'site, 'source> {
    Constructor(GoLiveNewtypeConstructorEntry<'site, 'source>),
    Projector(GoLiveNewtypeProjectorEntry<'site, 'source>),
    ExistentialProjector(GoLiveExistentialProjectorEntry<'site, 'source>),
}

impl<'site, 'source> GoLiveNewtypeMemberEntry<'site, 'source> {
    /// The one paired declaration-head entry owned by this capability.
    pub(crate) fn entry(&self) -> &GoLiveCallableEntry<'site, 'source> {
        match self {
            Self::Constructor(member) => member.entry(),
            Self::Projector(member) => member.entry(),
            Self::ExistentialProjector(member) => member.entry(),
        }
    }

    /// Exact flattened public value parameters for the member method.
    ///
    /// Declaration type heads remain execution-only nullary stages. An
    /// existential projector appends its validated continuation cursor to
    /// the declaration-owned receiver slots, making the public method the
    /// established `(receiver, continuation) -> R` shape rather than a
    /// method returning the intermediate CPS function.
    pub(crate) fn public_parameters(&self) -> Vec<&GoLiveFacadeUseRef<'site, 'source>> {
        let mut parameters = self
            .entry()
            .stages()
            .iter()
            .find_map(GoLiveHeadStage::value_stage)
            .expect("a validated newtype member owns one value head")
            .slots()
            .iter()
            .collect::<Vec<_>>();
        if let Self::ExistentialProjector(member) = self {
            parameters.push(member.continuation());
        }
        parameters
    }

    /// Exact public result cursor after member-specific flattening.
    pub(crate) fn result(&self) -> &GoLiveFacadeUseRef<'site, 'source> {
        match self {
            Self::Constructor(member) => member.entry().returned(),
            Self::Projector(member) => member.entry().returned(),
            Self::ExistentialProjector(member) => member.selected_result(),
        }
    }
}

/// Opaque proof that a live site is one validated constructor.
pub(crate) struct GoLiveNewtypeConstructorEntry<'site, 'source> {
    entry: GoLiveCallableEntry<'site, 'source>,
}

impl<'site, 'source> GoLiveNewtypeConstructorEntry<'site, 'source> {
    pub(crate) fn entry(&self) -> &GoLiveCallableEntry<'site, 'source> {
        &self.entry
    }
}

/// Opaque proof that a live site is one non-existential projector.
pub(crate) struct GoLiveNewtypeProjectorEntry<'site, 'source> {
    entry: GoLiveCallableEntry<'site, 'source>,
}

impl<'site, 'source> GoLiveNewtypeProjectorEntry<'site, 'source> {
    pub(crate) fn entry(&self) -> &GoLiveCallableEntry<'site, 'source> {
        &self.entry
    }
}

/// Opaque proof of an existential projector and its flattened public cut.
pub(crate) struct GoLiveExistentialProjectorEntry<'site, 'source> {
    entry: GoLiveCallableEntry<'site, 'source>,
    continuation: GoLiveFacadeUseRef<'site, 'source>,
    selected_result: GoLiveFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> GoLiveExistentialProjectorEntry<'site, 'source> {
    pub(crate) fn entry(&self) -> &GoLiveCallableEntry<'site, 'source> {
        &self.entry
    }

    pub(crate) fn continuation(&self) -> &GoLiveFacadeUseRef<'site, 'source> {
        &self.continuation
    }

    pub(crate) fn selected_result(&self) -> &GoLiveFacadeUseRef<'site, 'source> {
        &self.selected_result
    }
}

/// Live-only callable view. Retained sites cannot construct this type.
#[derive(Clone, Copy)]
pub(crate) struct GoLiveFacadeSite<'site, 'source> {
    site: &'site GoFacadeSite<'source>,
    execution: &'source CallableExecutionLayout,
}

impl<'site, 'source> GoLiveFacadeSite<'site, 'source> {
    pub(crate) fn site(&self) -> &'site GoFacadeSite<'source> {
        self.site
    }

    /// Pair the declaration-owned semantic cut with its exact execution cut.
    /// The returned cursor begins after every paired head, so those actions
    /// cannot also be consumed through generic per-use traversal.
    pub(crate) fn callable_entry(
        &self,
    ) -> Result<GoLiveCallableEntry<'site, 'source>, GoFacadeError> {
        let semantic = self.site.callable_entry()?;
        let execution = self.execution.head_stages();
        if semantic.stages.len() != execution.len() {
            return Err(GoFacadeError::new(
                "live semantic and execution callable heads have different lengths",
            ));
        }

        let mut stages = Vec::with_capacity(execution.len());
        for (semantic_stage, execution_stage) in semantic.stages.iter().zip(execution) {
            match execution_stage {
                CallableExecutionStage::Type { action } => {
                    let Some(semantic) = semantic_stage.type_stage() else {
                        return Err(GoFacadeError::new(
                            "live semantic and execution callable head kinds differ",
                        ));
                    };
                    stages.push(GoLiveHeadStage::Type(GoLiveTypeHeadStage {
                        binder: semantic.binder(),
                        action: *action,
                    }));
                }
                CallableExecutionStage::Value(layout) => {
                    let Some(semantic) = semantic_stage.value_stage() else {
                        return Err(GoFacadeError::new(
                            "live semantic and execution callable head kinds differ",
                        ));
                    };
                    stages.push(GoLiveHeadStage::Value(GoLiveValueHeadStage {
                        slots: semantic
                            .slots()
                            .iter()
                            .cloned()
                            .map(|slot| GoLiveFacadeUseRef::new(*self, slot))
                            .collect(),
                        layout,
                    }));
                }
            }
        }
        Ok(GoLiveCallableEntry {
            stages,
            returned: GoLiveFacadeUseRef::new(*self, semantic.returned),
        })
    }

    /// Classify and validate one live public newtype member from the exact
    /// owner-local nominal declaration paired with this site.
    ///
    /// Existentiality comes only from that declaration's existential binder
    /// inventory. The returned facade shape is then checked as evidence for
    /// the already-classified projector, never used to infer the class.
    pub(crate) fn newtype_member_entry(
        &self,
    ) -> Result<GoLiveNewtypeMemberEntry<'site, 'source>, GoFacadeError> {
        let (newtype, member, role) = match self.site.id().owner() {
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
                (newtype, member, GoNewtypeMemberRole::Constructor)
            }
            BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                (newtype, member, GoNewtypeMemberRole::Projector)
            }
            BoundaryFacadeSiteOwner::HostFunction { .. }
            | BoundaryFacadeSiteOwner::ExportedFunction { .. } => {
                return Err(GoFacadeError::new(
                    "a non-newtype facade site cannot mint a newtype-member capability",
                ));
            }
        };
        let name =
            QualifiedTypeName::new(self.site.id().module_segments().to_vec(), newtype.clone())
                .ok_or_else(|| {
                    GoFacadeError::new("a newtype member has an invalid owner identity")
                })?;
        let declaration = self
            .site
            .prepared
            .nominals()
            .declaration(&name)
            .ok_or_else(|| {
                GoFacadeError::new(format!(
                    "newtype member owner {}.{} is absent from its site-local nominal snapshot",
                    name.module_segments().join("."),
                    name.name()
                ))
            })?;
        let BoundaryNominalDeclaration::Newtype {
            type_params,
            existential_params,
            surface,
            ..
        } = declaration
        else {
            return Err(GoFacadeError::new(format!(
                "newtype member owner {}.{} resolves to a non-newtype nominal",
                name.module_segments().join("."),
                name.name()
            )));
        };
        let Some(realization) = self.site.nominals.get(&name) else {
            return Err(GoFacadeError::new(
                "newtype member owner is absent from its realized site-local nominal map",
            ));
        };
        if realization.arity() != type_params.len() {
            return Err(GoFacadeError::new(
                "newtype member owner and realized nominal disagree on universal arity",
            ));
        }
        if !newtype_surface_selects(surface, role, member) {
            return Err(GoFacadeError::new(format!(
                "newtype member {}.{}.{} disagrees with its site-local public surface",
                name.module_segments().join("."),
                name.name(),
                member
            )));
        }

        let mut expected_type_heads = type_params.iter().collect::<Vec<_>>();
        if role == GoNewtypeMemberRole::Constructor {
            expected_type_heads.extend(existential_params);
        }
        let entry = self.callable_entry()?;
        validate_newtype_member_heads(&entry, &expected_type_heads)?;

        match role {
            GoNewtypeMemberRole::Constructor => Ok(GoLiveNewtypeMemberEntry::Constructor(
                GoLiveNewtypeConstructorEntry { entry },
            )),
            GoNewtypeMemberRole::Projector if existential_params.is_empty() => Ok(
                GoLiveNewtypeMemberEntry::Projector(GoLiveNewtypeProjectorEntry { entry }),
            ),
            GoNewtypeMemberRole::Projector => {
                let (continuation, selected_result) =
                    self.validate_existential_projector_cut(&entry, existential_params)?;
                Ok(GoLiveNewtypeMemberEntry::ExistentialProjector(
                    GoLiveExistentialProjectorEntry {
                        entry,
                        continuation,
                        selected_result,
                    },
                ))
            }
        }
    }

    fn validate_existential_projector_cut(
        &self,
        entry: &GoLiveCallableEntry<'site, 'source>,
        existential_params: &[BoundaryNominalTypeParam],
    ) -> Result<
        (
            GoLiveFacadeUseRef<'site, 'source>,
            GoLiveFacadeUseRef<'site, 'source>,
        ),
        GoFacadeError,
    > {
        let (selected_binder, cps_function) = match entry.returned().view()? {
            GoLiveFacadeUseView::Forall { semantic, result } => {
                if semantic.binder_metadata().kind != crate::ast::Kind::Star {
                    return Err(GoFacadeError::new(
                        "existential projector selected-result binder is not kind-*",
                    ));
                }
                (semantic.binder(), result)
            }
            _ => {
                return Err(GoFacadeError::new(
                    "existential projector post-cut return is not exactly an outer Forall",
                ));
            }
        };

        let (continuation, selected_result) = match cps_function.view()? {
            GoLiveFacadeUseView::Function {
                layout,
                slots,
                result,
                ..
            } => {
                validate_single_source_function_layout(
                    layout,
                    slots.len(),
                    "existential projector outer CPS function",
                )?;
                let [continuation] = slots.as_slice() else {
                    return Err(GoFacadeError::new(
                        "existential projector outer CPS function does not have one continuation slot",
                    ));
                };
                (continuation.clone(), result)
            }
            _ => {
                return Err(GoFacadeError::new(
                    "existential projector outer Forall does not return one CPS function",
                ));
            }
        };
        self.require_selected_result(&selected_result, selected_binder)?;

        let mut payload_function = continuation.clone();
        for (index, expected) in existential_params.iter().enumerate() {
            payload_function = match payload_function.view()? {
                GoLiveFacadeUseView::Forall { semantic, result }
                    if semantic.binder_metadata().name == expected.name()
                        && &semantic.binder_metadata().kind == expected.kind() =>
                {
                    result
                }
                GoLiveFacadeUseView::Forall { .. } => {
                    return Err(GoFacadeError::new(format!(
                        "existential projector continuation binder {index} disagrees with its site-local nominal declaration"
                    )));
                }
                _ => {
                    return Err(GoFacadeError::new(format!(
                        "existential projector continuation is missing existential Forall {index}"
                    )));
                }
            };
        }

        let payload_result = match payload_function.view()? {
            GoLiveFacadeUseView::Function {
                layout,
                slots,
                result,
                ..
            } => {
                validate_existential_payload_function_layout(
                    layout,
                    slots.len(),
                    "existential projector continuation payload function",
                )?;
                result
            }
            _ => {
                return Err(GoFacadeError::new(
                    "existential projector continuation does not end in exactly one payload function",
                ));
            }
        };
        self.require_selected_result(&payload_result, selected_binder)?;

        Ok((continuation, selected_result))
    }

    fn require_selected_result(
        &self,
        cursor: &GoLiveFacadeUseRef<'site, 'source>,
        selected_binder: FacadeBinderId,
    ) -> Result<(), GoFacadeError> {
        let Some(source) = cursor.semantic.source() else {
            return Err(GoFacadeError::new(
                "existential projector selected result has no semantic source",
            ));
        };
        if source.arena() != &GoFacadeArenaId::Root {
            return Err(GoFacadeError::new(
                "existential projector selected result escaped its root facade arena",
            ));
        }
        if !matches!(
            self.site.use_at(&cursor.semantic.traversal.context)?,
            FacadeUse::Bound { binder, .. } if *binder == selected_binder
        ) {
            return Err(GoFacadeError::new(
                "existential projector CPS result is not bound by its selected-result Forall",
            ));
        }
        if !matches!(
            cursor.view()?,
            GoLiveFacadeUseView::Erased {
                reason: GoErasureReason::UnboundType,
                ..
            }
        ) {
            return Err(GoFacadeError::new(
                "existential projector selected-result cursor does not retain its live bound view",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GoNewtypeMemberRole {
    Constructor,
    Projector,
}

fn newtype_surface_selects(
    surface: &BoundaryNewtypeSurface,
    role: GoNewtypeMemberRole,
    member: &str,
) -> bool {
    match (role, surface) {
        (
            GoNewtypeMemberRole::Constructor,
            BoundaryNewtypeSurface::Constructor { member: actual },
        )
        | (
            GoNewtypeMemberRole::Constructor,
            BoundaryNewtypeSurface::Both {
                constructor: actual,
                ..
            },
        )
        | (GoNewtypeMemberRole::Projector, BoundaryNewtypeSurface::Projector { member: actual })
        | (
            GoNewtypeMemberRole::Projector,
            BoundaryNewtypeSurface::Both {
                projector: actual, ..
            },
        ) => actual == member,
        _ => false,
    }
}

fn validate_newtype_member_heads(
    entry: &GoLiveCallableEntry<'_, '_>,
    expected_type_heads: &[&BoundaryNominalTypeParam],
) -> Result<(), GoFacadeError> {
    if entry.stages().len() != expected_type_heads.len() + 1 {
        return Err(GoFacadeError::new(
            "newtype member declaration-head count disagrees with its site-local nominal declaration",
        ));
    }
    for (index, (stage, expected)) in entry.stages().iter().zip(expected_type_heads).enumerate() {
        let Some(stage) = stage.type_stage() else {
            return Err(GoFacadeError::new(format!(
                "newtype member declaration head {index} is not type-valued"
            )));
        };
        if stage.binder().name != expected.name() || &stage.binder().kind != expected.kind() {
            return Err(GoFacadeError::new(format!(
                "newtype member declaration type head {index} disagrees with its owner binder"
            )));
        }
        stage.require_invoke_nullary();
    }

    let Some(value_stage) = entry.stages().last().and_then(GoLiveHeadStage::value_stage) else {
        return Err(GoFacadeError::new(
            "newtype member declaration does not end in one value head",
        ));
    };
    if value_stage.source_groups()?.len() != 1 {
        return Err(GoFacadeError::new(
            "newtype member declaration value head does not own exactly one source parameter",
        ));
    }
    Ok(())
}

fn validate_single_source_function_layout(
    layout: &CallableValueStageLayout,
    facade_slot_count: usize,
    context: &str,
) -> Result<(), GoFacadeError> {
    if layout.source_param_count() != 1
        || layout.body_abi_arity() != 1
        || layout.facade_slot_count() != facade_slot_count
        || layout.source_params().len() != 1
    {
        return Err(GoFacadeError::new(format!(
            "{context} does not retain one exact source parameter"
        )));
    }
    let source = &layout.source_params()[0];
    let expected_adapter = match facade_slot_count {
        0 => CallableSourceParamAdapter::UnitValue,
        1 => CallableSourceParamAdapter::Identity,
        _ => CallableSourceParamAdapter::RightNest,
    };
    if source.facade_slots() != (0..facade_slot_count) || source.adapter() != expected_adapter {
        return Err(GoFacadeError::new(format!(
            "{context} source adapter disagrees with its exact facade slots"
        )));
    }
    Ok(())
}

fn validate_existential_payload_function_layout(
    layout: &CallableValueStageLayout,
    facade_slot_count: usize,
    context: &str,
) -> Result<(), GoFacadeError> {
    if layout.source_param_count() == 0 {
        if layout.body_abi_arity() == 0
            && facade_slot_count == 0
            && layout.facade_slot_count() == 0
            && layout.source_params().is_empty()
        {
            return Ok(());
        }
        return Err(GoFacadeError::new(format!(
            "{context} nullary source layout disagrees with its exact facade slots"
        )));
    }
    validate_single_source_function_layout(layout, facade_slot_count, context)
}

/// One declaration-owned live head with semantic and execution facts paired.
pub(crate) enum GoLiveHeadStage<'site, 'source> {
    Type(GoLiveTypeHeadStage<'source>),
    Value(GoLiveValueHeadStage<'site, 'source>),
}

/// Opaque capability for one declaration-owned live type head.
///
/// Its private action cannot be detached and used to invoke a retained or
/// independently assembled stage. Holding this value proves that the action
/// came from the same live entry as its binder metadata.
pub(crate) struct GoLiveTypeHeadStage<'source> {
    binder: &'source FacadeBinder,
    action: CallableTypeStageAction,
}

impl<'source> GoLiveTypeHeadStage<'source> {
    pub(crate) fn binder(&self) -> &'source FacadeBinder {
        self.binder
    }

    /// Validate the only execution operation this opaque capability admits.
    ///
    /// The exhaustive match deliberately makes a new action variant a
    /// compile-time design decision instead of silently widening invocation.
    pub(crate) fn require_invoke_nullary(&self) {
        match self.action {
            CallableTypeStageAction::InvokeNullary => {}
        }
    }
}

/// Opaque capability for one declaration-owned live value head.
///
/// Slots stay readable for public signature rendering. The ABI layout never
/// leaves the capability: callers can only ask for validated source groups
/// whose adapter is already bound to this head's exact slot slice.
pub(crate) struct GoLiveValueHeadStage<'site, 'source> {
    slots: Vec<GoLiveFacadeUseRef<'site, 'source>>,
    layout: &'source CallableValueStageLayout,
}

impl<'site, 'source> GoLiveValueHeadStage<'site, 'source> {
    pub(crate) fn slots(&self) -> &[GoLiveFacadeUseRef<'site, 'source>] {
        &self.slots
    }

    pub(crate) fn source_groups(
        &self,
    ) -> Result<Vec<GoLiveValueSourceGroup<'_, 'site, 'source>>, GoFacadeError> {
        if self.layout.facade_slot_count() != self.slots.len()
            || self.layout.source_param_count() != self.layout.body_abi_arity()
            || self.layout.source_params().len() != self.layout.body_abi_arity()
        {
            return Err(GoFacadeError::new(
                "live value-head layout disagrees with its paired slot inventory",
            ));
        }

        self.layout
            .source_params()
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let range = source.facade_slots();
                let slots = self.slots.get(range).ok_or_else(|| {
                    GoFacadeError::new(format!(
                        "live value-head source group {index} is outside its paired slots"
                    ))
                })?;
                let kind = match source.adapter() {
                    CallableSourceParamAdapter::UnitValue if slots.is_empty() => {
                        GoLiveValueSourceGroupKind::UnitValue
                    }
                    CallableSourceParamAdapter::Identity if slots.len() == 1 => {
                        GoLiveValueSourceGroupKind::Identity(&slots[0])
                    }
                    CallableSourceParamAdapter::RightNest if slots.len() > 1 => {
                        GoLiveValueSourceGroupKind::RightNest(slots)
                    }
                    _ => {
                        return Err(GoFacadeError::new(format!(
                            "live value-head source group {index} disagrees with its adapter"
                        )));
                    }
                };
                Ok(GoLiveValueSourceGroup { kind })
            })
            .collect()
    }
}

/// One source parameter with its adapter inseparably bound to exact live
/// facade slots.
pub(crate) struct GoLiveValueSourceGroup<'head, 'site, 'source> {
    kind: GoLiveValueSourceGroupKind<'head, 'site, 'source>,
}

enum GoLiveValueSourceGroupKind<'head, 'site, 'source> {
    UnitValue,
    Identity(&'head GoLiveFacadeUseRef<'site, 'source>),
    RightNest(&'head [GoLiveFacadeUseRef<'site, 'source>]),
}

/// Validated topology of one opaque value-head source group.
pub(crate) enum GoLiveValueSourceGroupView<'head, 'site, 'source> {
    UnitValue,
    Identity(&'head GoLiveFacadeUseRef<'site, 'source>),
    RightNest(&'head [GoLiveFacadeUseRef<'site, 'source>]),
}

impl<'head, 'site, 'source> GoLiveValueSourceGroup<'head, 'site, 'source> {
    pub(crate) fn view(&self) -> GoLiveValueSourceGroupView<'_, 'site, 'source> {
        match &self.kind {
            GoLiveValueSourceGroupKind::UnitValue => GoLiveValueSourceGroupView::UnitValue,
            GoLiveValueSourceGroupKind::Identity(slot) => {
                GoLiveValueSourceGroupView::Identity(slot)
            }
            GoLiveValueSourceGroupKind::RightNest(slots) => {
                GoLiveValueSourceGroupView::RightNest(slots)
            }
        }
    }
}

/// Paired callable heads and the exact cursor after their cut.
pub(crate) struct GoLiveCallableEntry<'site, 'source> {
    stages: Vec<GoLiveHeadStage<'site, 'source>>,
    returned: GoLiveFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> GoLiveCallableEntry<'site, 'source> {
    pub(crate) fn stages(&self) -> &[GoLiveHeadStage<'site, 'source>] {
        &self.stages
    }

    pub(crate) fn returned(&self) -> &GoLiveFacadeUseRef<'site, 'source> {
        &self.returned
    }
}

impl<'site, 'source> GoLiveHeadStage<'site, 'source> {
    pub(crate) fn type_stage(&self) -> Option<&GoLiveTypeHeadStage<'source>> {
        match self {
            Self::Type(stage) => Some(stage),
            Self::Value(_) => None,
        }
    }

    pub(crate) fn value_stage(&self) -> Option<&GoLiveValueHeadStage<'site, 'source>> {
        match self {
            Self::Value(stage) => Some(stage),
            Self::Type(_) => None,
        }
    }
}

/// Complete pure Go facade realization over one prepared boundary catalog.
///
/// The catalog borrows the preparation transaction. A [`GoFacadeSite`]
/// retains the exact [`PreparedBoundaryCallableSite`] from that transaction,
/// so live execution and semantic arenas cannot be independently rejoined.
/// The outer package compositor owns the prepared transaction and this
/// borrowing catalog as adjacent values; the catalog deliberately does not
/// attempt unsafe or self-referential ownership.
pub(crate) struct GoFacadeCatalog<'a> {
    prepared: &'a PreparedBoundaryCallableSites,
    host_bindings: GoHostBindingPlan,
    sites: BTreeMap<BoundaryFacadeSiteId, GoFacadeSite<'a>>,
    shells: BTreeMap<FacadeShellId, GoFacadeShell>,
    sum_keys: BTreeMap<SemanticKey, GoSumKey>,
    carriers: BTreeMap<QualifiedTypeName, GoNewtypeCarrier>,
    shell_provenance: BTreeMap<FacadeShellId, GoDeclarationProvenance>,
    sum_key_provenance: BTreeMap<SemanticKey, GoDeclarationProvenance>,
    carrier_provenance: BTreeMap<QualifiedTypeName, GoDeclarationProvenance>,
    claims: GoFacadeClaims,
}

impl<'a> GoFacadeCatalog<'a> {
    pub(crate) fn prepare(
        prepared: &'a PreparedBoundaryCallableSites,
    ) -> Result<Self, GoFacadeError> {
        let host_bindings = GoHostBindingPlan::prepare(prepared.host_bindings())?;
        let mut sites = BTreeMap::new();
        let mut shell_provenance = BTreeMap::new();
        let mut sum_key_provenance = BTreeMap::new();
        let mut carrier_provenance = BTreeMap::new();
        for prepared_site in prepared.sites() {
            let site = GoFacadeSite::prepare(prepared_site, &host_bindings)?;
            if sites.insert(site.id().clone(), site).is_some() {
                return Err(GoFacadeError::new(
                    "one prepared Go facade site occurred more than once",
                ));
            }
        }

        let prepared_root_shells = prepared.shells().cloned().collect::<BTreeSet<_>>();
        let mut reachable = GoReachability::default();
        for site in sites.values_mut() {
            let mut site_reachable = site.collect_reachability()?;
            let provenance = site.provenance();
            for shell in &site_reachable.shells {
                record_declaration_provenance(&mut shell_provenance, shell, provenance);
                if shell.kind() == FacadeKind::Sum {
                    for key in shell.ordered_keys() {
                        record_declaration_provenance(&mut sum_key_provenance, key, provenance);
                    }
                }
            }
            for carrier in &site_reachable.carriers {
                record_declaration_provenance(&mut carrier_provenance, carrier.name(), provenance);
            }
            let contextual_sums = std::mem::take(&mut site_reachable.contextual_sums);
            site.install_contextual_sum_rows(contextual_sums, &host_bindings.arguments())?;
            reachable.merge(site_reachable)?;
        }

        // The prepared catalogs are authority inventories, not eager Go
        // reachability. In particular, ignored transparent arguments remain
        // absent from `reachable`. Validate only that the lazy walk selected
        // identities already owned by a root plan or a nominal payload.
        let payload_shells = sites
            .values()
            .flat_map(|site| site.nominals.values())
            .filter_map(|nominal| match nominal {
                GoNominalPlan::Transparent { payload, .. } => Some(payload),
                _ => None,
            })
            .flat_map(|payload| payload.shell_dependencies())
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(unowned) = reachable.shells.iter().find(|shell| {
            !prepared_root_shells.contains(*shell) && !payload_shells.contains(*shell)
        }) {
            return Err(GoFacadeError::new(format!(
                "Go reachability selected an unprepared {:?} facade shell",
                unowned.kind()
            )));
        }

        // Public inventories, unlike the lazy Go walk, also name nominal
        // carriers that an ignored transparent argument can leave unvisited.
        for entry in prepared.public_newtypes() {
            if entry.surface().uses_nominal_carrier() {
                record_declaration_provenance(
                    &mut carrier_provenance,
                    entry.name(),
                    GoDeclarationProvenance::Live,
                );
                reachable
                    .carriers
                    .insert(GoNewtypeCarrier::new(entry.name())?);
            }
        }
        for entry in prepared.retained_public_newtypes() {
            if entry.surface().uses_nominal_carrier() {
                let removed_at_version = prepared
                    .retained_public_newtype_removed_at(entry.name())
                    .expect("a retained Go newtype carrier has removal provenance");
                record_declaration_provenance(
                    &mut carrier_provenance,
                    entry.name(),
                    GoDeclarationProvenance::Retained { removed_at_version },
                );
                reachable
                    .carriers
                    .insert(GoNewtypeCarrier::new(entry.name())?);
            }
        }

        let shells = reachable
            .shells
            .iter()
            .map(|id| Ok((id.clone(), GoFacadeShell::new(id)?)))
            .collect::<Result<BTreeMap<_, _>, GoFacadeError>>()?;
        let mut sum_keys = BTreeMap::new();
        for shell in shells.values() {
            for key in &shell.sum_keys {
                sum_keys
                    .entry(key.key.clone())
                    .or_insert_with(|| key.clone());
            }
        }
        let carriers = reachable
            .carriers
            .into_iter()
            .map(|carrier| (carrier.name().clone(), carrier))
            .collect::<BTreeMap<_, _>>();
        let claims = build_facade_claims(&sites, &shells, &sum_keys, &carriers)?;

        Ok(Self {
            prepared,
            host_bindings,
            sites,
            shells,
            sum_keys,
            carriers,
            shell_provenance,
            sum_key_provenance,
            carrier_provenance,
            claims,
        })
    }

    pub(crate) fn site(&self, id: &BoundaryFacadeSiteId) -> Option<&GoFacadeSite<'a>> {
        self.sites.get(id)
    }

    pub(crate) fn host_binding_declaration(&self) -> String {
        self.host_bindings.declaration()
    }

    pub(crate) fn host_binding_arguments(&self) -> String {
        self.host_bindings.arguments()
    }

    pub(crate) fn role_adapters(&self) -> impl ExactSizeIterator<Item = &GoRoleAdapter> {
        self.host_bindings.role_adapters()
    }

    pub(crate) fn parametric_host_carriers(
        &self,
    ) -> impl ExactSizeIterator<Item = &GoHostTypeCarrier> {
        self.host_bindings.parametric_carriers()
    }

    pub(crate) fn retained_host_types(&self) -> impl ExactSizeIterator<Item = &GoRetainedHostType> {
        self.host_bindings.retained_types()
    }

    pub(crate) fn sites(
        &self,
    ) -> impl DoubleEndedIterator<Item = &GoFacadeSite<'a>> + ExactSizeIterator {
        self.sites.values()
    }

    pub(crate) fn live_sites(&self) -> impl DoubleEndedIterator<Item = GoLiveFacadeSite<'_, 'a>> {
        self.sites.values().filter_map(GoFacadeSite::live)
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn shells(
        &self,
    ) -> impl DoubleEndedIterator<Item = &GoFacadeShell> + ExactSizeIterator {
        self.shells.values()
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn carriers(
        &self,
    ) -> impl DoubleEndedIterator<Item = &GoNewtypeCarrier> + ExactSizeIterator {
        self.carriers.values()
    }

    /// Live bridged public-newtype inventory. Retained nominal closures never
    /// enter this iterator and therefore cannot create package handles or
    /// callable members.
    pub(crate) fn public_newtypes(
        &self,
    ) -> impl DoubleEndedIterator<Item = &BoundaryPublicNewtypeInventoryEntry> + ExactSizeIterator
    {
        self.prepared.public_newtypes()
    }

    pub(crate) fn claims(&self) -> &GoFacadeClaims {
        &self.claims
    }

    /// Render declarations after the package compositor has merged and
    /// validated the complete package claim inventory.
    pub(crate) fn render_declarations(&self) -> Result<String, GoFacadeError> {
        let mut out = String::new();
        render_sum_support(&mut out);
        for retained in self.retained_host_types() {
            retained.render_declaration(&mut out);
        }
        for carrier in self.parametric_host_carriers() {
            carrier.render_declaration(&mut out);
        }
        for carrier in self.carriers.values() {
            carrier.render_declaration_with_provenance(
                &mut out,
                self.carrier_provenance
                    .get(carrier.name())
                    .copied()
                    .unwrap_or(GoDeclarationProvenance::Live),
            );
        }
        for key in self.sum_keys.values() {
            key.render_declaration_with_provenance(
                &mut out,
                self.sum_key_provenance
                    .get(&key.key)
                    .copied()
                    .unwrap_or(GoDeclarationProvenance::Live),
            );
        }
        for shell in self.shells.values() {
            shell.render_declaration_with_provenance(
                &mut out,
                self.shell_provenance
                    .get(shell.id())
                    .copied()
                    .unwrap_or(GoDeclarationProvenance::Live),
            )?;
        }
        for site in self.sites.values() {
            let provenance = match site.prepared.origin() {
                crate::backends::boundary_facade::PreparedBoundaryCallableOriginRef::Live(_) => {
                    GoDeclarationProvenance::Live
                }
                crate::backends::boundary_facade::PreparedBoundaryCallableOriginRef::Retained(
                    metadata,
                ) => GoDeclarationProvenance::Retained {
                    removed_at_version: metadata.removed_at_version(),
                },
            };
            for row in site.contextual_sum_rows.values() {
                render_deprecation_comment(&mut out, provenance.removed_at_version());
                writeln!(
                    out,
                    "type {}{} = {}\n",
                    row.name,
                    self.host_binding_declaration(),
                    row.concrete_type
                )
                .expect("writing to String cannot fail");
            }
        }
        Ok(out)
    }
}

fn build_facade_claims(
    sites: &BTreeMap<BoundaryFacadeSiteId, GoFacadeSite<'_>>,
    shells: &BTreeMap<FacadeShellId, GoFacadeShell>,
    sum_keys: &BTreeMap<SemanticKey, GoSumKey>,
    carriers: &BTreeMap<QualifiedTypeName, GoNewtypeCarrier>,
) -> Result<GoFacadeClaims, GoFacadeError> {
    let mut claims = vec![
        (
            GoFacadeScope::Package,
            go_identifier(UNIT_TYPE.to_owned())?,
            GoFacadeNameOwner::Support(GoFacadeSupportName::Unit),
        ),
        (
            GoFacadeScope::Package,
            go_identifier(SUM_CARRIER_TYPE.to_owned())?,
            GoFacadeNameOwner::Support(GoFacadeSupportName::SumCarrier),
        ),
        (
            GoFacadeScope::Package,
            go_identifier(SUM_CASE_MARKER.to_owned())?,
            GoFacadeNameOwner::Support(GoFacadeSupportName::SumCaseMarker),
        ),
        (
            GoFacadeScope::Package,
            go_identifier(SUM_ROW_CONSTRAINT.to_owned())?,
            GoFacadeNameOwner::Support(GoFacadeSupportName::SumRowConstraint),
        ),
    ];

    for shell in shells.values() {
        claims.push((
            GoFacadeScope::Package,
            shell.name.clone(),
            GoFacadeNameOwner::Shell {
                shell: shell.id.clone(),
                role: GoShellNameRole::Shell,
            },
        ));
        match shell.id.kind() {
            FacadeKind::Product => {
                for (key, name) in shell.id.ordered_keys().iter().zip(&shell.fields) {
                    claims.push((
                        GoFacadeScope::ProductFields(shell.id.clone()),
                        name.clone(),
                        GoFacadeNameOwner::ProductField {
                            shell: shell.id.clone(),
                            key: key.clone(),
                        },
                    ));
                }
            }
            FacadeKind::Sum => {
                claims.push((
                    GoFacadeScope::Package,
                    shell.row_name.clone().expect("a sum shell has a row name"),
                    GoFacadeNameOwner::Shell {
                        shell: shell.id.clone(),
                        role: GoShellNameRole::SumRow,
                    },
                ));
            }
        }
    }

    for helper in sum_keys.values() {
        for (name, role) in [
            (&helper.slot_witness, GoSumKeyNameRole::SlotWitness),
            (&helper.row_constraint, GoSumKeyNameRole::RowConstraint),
            (&helper.public_case, GoSumKeyNameRole::PublicCase),
            (&helper.private_case, GoSumKeyNameRole::PrivateCase),
            (&helper.constructor, GoSumKeyNameRole::Constructor),
        ] {
            claims.push((
                GoFacadeScope::Package,
                name.clone(),
                GoFacadeNameOwner::SumKey {
                    key: helper.key.clone(),
                    role,
                },
            ));
        }
    }

    for carrier in carriers.values() {
        claims.push((
            GoFacadeScope::Package,
            carrier.public_type.clone(),
            GoFacadeNameOwner::Carrier {
                name: carrier.name.clone(),
                role: GoCarrierNameRole::PublicType,
            },
        ));
    }

    for site in sites.values() {
        for row in site.contextual_sum_rows.values() {
            claims.push((
                GoFacadeScope::Package,
                row.name.clone(),
                GoFacadeNameOwner::ContextualSumRow {
                    site: site.id().clone(),
                    ordinal: row.ordinal,
                },
            ));
        }
    }

    ScopeClaims::try_collect(claims).map_err(|collision| {
        GoFacadeError::new(format!(
            "Go facade namespace collision at {:?} name {} between {:?} and {:?}",
            collision.scope(),
            collision.name(),
            collision.first_owner(),
            collision.second_owner()
        ))
    })
}

fn render_sum_support(out: &mut String) {
    writeln!(out, "type {SUM_CASE_MARKER} interface {{").expect("writing to String cannot fail");
    writeln!(out, "\tkioCaseMarker()").expect("writing to String cannot fail");
    writeln!(out, "}}\n").expect("writing to String cannot fail");
    writeln!(out, "type {SUM_ROW_CONSTRAINT}[R any] interface {{")
        .expect("writing to String cannot fail");
    writeln!(out, "\tzeroCase(R) {SUM_CASE_MARKER}").expect("writing to String cannot fail");
    writeln!(out, "}}\n").expect("writing to String cannot fail");
    writeln!(
        out,
        "type {SUM_CARRIER_TYPE}[R {SUM_ROW_CONSTRAINT}[R]] struct {{"
    )
    .expect("writing to String cannot fail");
    writeln!(out, "\tphantom [0]*R").expect("writing to String cannot fail");
    writeln!(out, "\tstored {SUM_CASE_MARKER}").expect("writing to String cannot fail");
    writeln!(out, "}}\n").expect("writing to String cannot fail");
    writeln!(
        out,
        "func (value {SUM_CARRIER_TYPE}[R]) Case() {SUM_CASE_MARKER} {{"
    )
    .expect("writing to String cannot fail");
    writeln!(out, "\tif value.stored != nil {{").expect("writing to String cannot fail");
    writeln!(out, "\t\treturn value.stored").expect("writing to String cannot fail");
    writeln!(out, "\t}}").expect("writing to String cannot fail");
    writeln!(out, "\tvar row R").expect("writing to String cannot fail");
    writeln!(out, "\treturn row.zeroCase(row)").expect("writing to String cannot fail");
    writeln!(out, "}}\n").expect("writing to String cannot fail");
}

impl GoFacadeShell {
    #[cfg(test)]
    fn render_declaration(&self, out: &mut String) -> Result<(), GoFacadeError> {
        self.render_declaration_with_provenance(out, GoDeclarationProvenance::Live)
    }

    fn render_declaration_with_provenance(
        &self,
        out: &mut String,
        provenance: GoDeclarationProvenance,
    ) -> Result<(), GoFacadeError> {
        let declaration_params = go_type_parameter_declaration(self.id.ordered_keys().len());
        let use_params = go_type_parameter_use(self.id.ordered_keys().len());
        match self.id.kind() {
            FacadeKind::Product => {
                render_deprecation_comment(out, provenance.removed_at_version());
                writeln!(out, "type {}{declaration_params} struct {{", self.name)
                    .expect("writing to String cannot fail");
                for (index, field) in self.fields.iter().enumerate() {
                    render_deprecation_comment_with_indent(
                        out,
                        provenance.removed_at_version(),
                        "\t",
                    );
                    writeln!(out, "\t{field} T{index}").expect("writing to String cannot fail");
                }
                writeln!(out, "}}\n").expect("writing to String cannot fail");
            }
            FacadeKind::Sum => {
                let row = self.row_name.as_ref().expect("a sum shell has a row name");
                let first = self
                    .sum_keys
                    .first()
                    .ok_or_else(|| GoFacadeError::new("a reachable sum shell has no arms"))?;
                render_deprecation_comment(out, provenance.removed_at_version());
                writeln!(out, "type {row}{declaration_params} struct {{")
                    .expect("writing to String cannot fail");
                for (index, helper) in self.sum_keys.iter().enumerate() {
                    writeln!(out, "\t{}[T{index}]", helper.slot_witness)
                        .expect("writing to String cannot fail");
                }
                writeln!(out, "}}\n").expect("writing to String cannot fail");
                render_deprecation_comment(out, provenance.removed_at_version());
                writeln!(
                    out,
                    "func ({row}{use_params}) zeroCase(_ {row}{use_params}) {SUM_CASE_MARKER} {{"
                )
                .expect("writing to String cannot fail");
                writeln!(
                    out,
                    "\treturn {}[{row}{use_params}, T0]{{}}",
                    first.private_case
                )
                .expect("writing to String cannot fail");
                writeln!(out, "}}\n").expect("writing to String cannot fail");
                render_deprecation_comment(out, provenance.removed_at_version());
                writeln!(
                    out,
                    "type {}{declaration_params} = {SUM_CARRIER_TYPE}[{row}{use_params}]\n",
                    self.name
                )
                .expect("writing to String cannot fail");
            }
        }
        Ok(())
    }
}

fn go_type_parameter_declaration(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        let names = (0..count)
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{names} any]")
    }
}

fn go_type_parameter_use(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        let names = (0..count)
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{names}]")
    }
}

fn go_role_type(role: Role) -> &'static str {
    match role {
        Role::I8 => "int8",
        Role::I16 => "int16",
        Role::I32 => "int32",
        Role::I64 => "int64",
        Role::I128 | Role::U128 => "*big.Int",
        Role::U8 => "uint8",
        Role::U16 => "uint16",
        Role::U32 => "uint32",
        Role::U64 => "uint64",
        Role::F32 => "float32",
        Role::F64 => "float64",
        Role::Bool => "bool",
        Role::Str => "string",
    }
}

/// Borrowed semantic arena with one declaration-binder substitution context.
struct GoFacadeArenaRef<'site, 'source> {
    site: &'site GoFacadeSite<'source>,
    arena: GoFacadeArenaId,
    substitutions: Arc<BTreeMap<FacadeBinderId, GoFacadeContext>>,
    bound_args: Arc<BTreeMap<FacadeBinderId, GoFacadeTraversal>>,
    active_newtypes: Arc<Vec<QualifiedTypeName>>,
}

impl<'site, 'source> Clone for GoFacadeArenaRef<'site, 'source> {
    fn clone(&self) -> Self {
        Self {
            site: self.site,
            arena: self.arena.clone(),
            substitutions: Arc::clone(&self.substitutions),
            bound_args: Arc::clone(&self.bound_args),
            active_newtypes: Arc::clone(&self.active_newtypes),
        }
    }
}

impl<'site, 'source> GoFacadeArenaRef<'site, 'source> {
    fn plan(&self) -> Result<&'site BoundaryFacadePlan, GoFacadeError> {
        self.site.arena_plan(&self.arena)
    }

    fn root_use(&self) -> Result<GoFacadeUseRef<'site, 'source>, GoFacadeError> {
        let use_id = match &self.arena {
            GoFacadeArenaId::Root => self.site.callable().facade().root(),
            GoFacadeArenaId::TransparentPayload(name) => {
                let (_, payload) = self.site.payload_plan(name)?;
                payload.payload_root()
            }
        };
        self.use_ref(use_id)
    }

    fn use_ref(
        &self,
        use_id: FacadeUseId,
    ) -> Result<GoFacadeUseRef<'site, 'source>, GoFacadeError> {
        let plan = self.plan()?;
        if plan.uses().get(use_id.index()).is_none() {
            return Err(GoFacadeError::new(
                "facade use ID is outside the requested semantic arena",
            ));
        }
        Ok(GoFacadeUseRef {
            site: self.site,
            traversal: GoFacadeTraversal {
                context: GoFacadeContext::use_in(
                    self.arena.clone(),
                    use_id,
                    Arc::clone(&self.substitutions),
                ),
                bound_args: Arc::clone(&self.bound_args),
                active_newtypes: Arc::clone(&self.active_newtypes),
            },
        })
    }
}

/// Opaque contextual facade-use cursor.
///
/// The cursor retains source arena/use identity and term-valued declaration
/// substitutions. It can therefore substitute a higher-kinded constructor and
/// later apply it without prematurely rendering either side as `any`.
pub(crate) struct GoFacadeUseRef<'site, 'source> {
    site: &'site GoFacadeSite<'source>,
    traversal: GoFacadeTraversal,
}

impl<'site, 'source> Clone for GoFacadeUseRef<'site, 'source> {
    fn clone(&self) -> Self {
        Self {
            site: self.site,
            traversal: self.traversal.clone(),
        }
    }
}

impl<'site, 'source> GoFacadeUseRef<'site, 'source> {
    pub(crate) fn source(&self) -> Option<&GoFacadeUseSource> {
        match self.traversal.context.term.as_ref() {
            GoFacadeTerm::Use { source, .. } => Some(source),
            GoFacadeTerm::Erased(_) => None,
        }
    }

    pub(crate) fn boundary_type(&self) -> Result<GoType, GoFacadeError> {
        let mut ignored = GoReachability::default();
        match self.site.render_context(&self.traversal, &mut ignored)? {
            GoRenderResult::Complete(complete) => Ok(complete.ty),
            GoRenderResult::Cycle(_) => Ok(GoType::new(ERASED_TYPE)),
        }
    }

    pub(crate) fn view(&self) -> Result<GoFacadeUseView<'site, 'source>, GoFacadeError> {
        self.site.view_context(&self.traversal)
    }
}

/// Opaque live post-cut cursor. Only paired callable heads and descendants
/// can mint this capability, so declaration heads cannot be consumed once as
/// stages and again as generic execution uses.
pub(crate) struct GoLiveFacadeUseRef<'site, 'source> {
    live: GoLiveFacadeSite<'site, 'source>,
    semantic: GoFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> Clone for GoLiveFacadeUseRef<'site, 'source> {
    fn clone(&self) -> Self {
        Self {
            live: self.live,
            semantic: self.semantic.clone(),
        }
    }
}

impl<'site, 'source> GoLiveFacadeUseRef<'site, 'source> {
    fn new(
        live: GoLiveFacadeSite<'site, 'source>,
        semantic: GoFacadeUseRef<'site, 'source>,
    ) -> Self {
        debug_assert!(std::ptr::eq(live.site, semantic.site));
        Self { live, semantic }
    }

    #[cfg(all(test, feature = "surface"))]
    fn unresolved_source(&self) -> Option<&GoFacadeUseSource> {
        self.semantic.source()
    }

    pub(crate) fn boundary_type(&self) -> Result<GoType, GoFacadeError> {
        self.semantic.boundary_type()
    }

    /// Borrow the paired type/name cursor without transferring live
    /// execution authority. Semantic alias walkers can reuse their one tree
    /// grammar through this view; conversion and invocation still require
    /// the enclosing live cursor.
    pub(crate) fn semantic(&self) -> &GoFacadeUseRef<'site, 'source> {
        &self.semantic
    }

    pub(crate) fn view(&self) -> Result<GoLiveFacadeUseView<'site, 'source>, GoFacadeError> {
        let live = self.live;
        Ok(match self.semantic.view()? {
            GoFacadeUseView::Unit => GoLiveFacadeUseView::Unit,
            GoFacadeUseView::Bottom => GoLiveFacadeUseView::Bottom,
            GoFacadeUseView::Erased { reason } => GoLiveFacadeUseView::Erased { reason },
            GoFacadeUseView::ExactHost {
                boundary_type,
                role_adapter,
            } => GoLiveFacadeUseView::ExactHost {
                boundary_type,
                role_adapter,
            },
            GoFacadeUseView::HostCarrier {
                boundary_type,
                carrier,
            } => GoLiveFacadeUseView::HostCarrier {
                boundary_type,
                carrier,
            },
            GoFacadeUseView::Carrier { carrier } => GoLiveFacadeUseView::Carrier { carrier },
            GoFacadeUseView::Transparent { payload } => GoLiveFacadeUseView::Transparent {
                payload: GoLiveFacadeUseRef::new(live, payload),
            },
            GoFacadeUseView::Product(product) => {
                let fields = product
                    .fields()
                    .iter()
                    .map(|field| GoLiveFacadeUseRef::new(live, field.payload().clone()))
                    .collect();
                GoLiveFacadeUseView::Product {
                    semantic: product,
                    fields,
                }
            }
            GoFacadeUseView::Sum(sum) => {
                let arms = sum
                    .arms()
                    .iter()
                    .map(|arm| GoLiveFacadeUseRef::new(live, arm.payload().clone()))
                    .collect();
                GoLiveFacadeUseView::Sum {
                    semantic: sum,
                    arms,
                }
            }
            GoFacadeUseView::Function(function) => {
                let layout = match live.site.execution_use(function.source()) {
                    Some(BoundaryFacadeExecutionUse::Function(layout)) => layout,
                    Some(_) => {
                        return Err(GoFacadeError::new(
                            "a live Function facade use is not paired with a Function action",
                        ));
                    }
                    None => {
                        return Err(GoFacadeError::new(
                            "a live Function facade use is missing its execution action",
                        ));
                    }
                };
                let slots = function
                    .slots()
                    .iter()
                    .cloned()
                    .map(|slot| GoLiveFacadeUseRef::new(live, slot))
                    .collect();
                let result = GoLiveFacadeUseRef::new(live, function.result().clone());
                GoLiveFacadeUseView::Function {
                    semantic: function,
                    layout,
                    slots,
                    result,
                }
            }
            GoFacadeUseView::Forall(forall) => {
                match live.site.execution_use(forall.source()) {
                    Some(BoundaryFacadeExecutionUse::InvokeForall) => {}
                    Some(_) => {
                        return Err(GoFacadeError::new(
                            "a live Forall facade use is not paired with an InvokeForall action",
                        ));
                    }
                    None => {
                        return Err(GoFacadeError::new(
                            "a live Forall facade use is missing its execution action",
                        ));
                    }
                }
                let result = GoLiveFacadeUseRef::new(live, forall.result().clone());
                GoLiveFacadeUseView::Forall {
                    semantic: forall,
                    result,
                }
            }
        })
    }
}

/// Exhaustive live view. Structural metadata stays in the semantic view while
/// every recursive edge is separately exposed as an unforgeable live cursor.
pub(crate) enum GoLiveFacadeUseView<'site, 'source> {
    Unit,
    Bottom,
    Erased {
        reason: GoErasureReason,
    },
    ExactHost {
        boundary_type: GoType,
        role_adapter: Option<GoRoleAdapter>,
    },
    HostCarrier {
        boundary_type: GoType,
        carrier: GoHostTypeCarrier,
    },
    Carrier {
        carrier: GoNewtypeCarrier,
    },
    Transparent {
        payload: GoLiveFacadeUseRef<'site, 'source>,
    },
    Product {
        semantic: GoProductUse<'site, 'source>,
        fields: Vec<GoLiveFacadeUseRef<'site, 'source>>,
    },
    Sum {
        semantic: GoSumUse<'site, 'source>,
        arms: Vec<GoLiveFacadeUseRef<'site, 'source>>,
    },
    Function {
        semantic: GoFunctionUse<'site, 'source>,
        layout: &'source CallableValueStageLayout,
        slots: Vec<GoLiveFacadeUseRef<'site, 'source>>,
        result: GoLiveFacadeUseRef<'site, 'source>,
    },
    Forall {
        semantic: GoForallUse<'site, 'source>,
        result: GoLiveFacadeUseRef<'site, 'source>,
    },
}

/// Exhaustive contextual facade use seen by the conversion layer.
pub(crate) enum GoFacadeUseView<'site, 'source> {
    Unit,
    Bottom,
    Erased {
        reason: GoErasureReason,
    },
    ExactHost {
        boundary_type: GoType,
        role_adapter: Option<GoRoleAdapter>,
    },
    HostCarrier {
        boundary_type: GoType,
        carrier: GoHostTypeCarrier,
    },
    Carrier {
        carrier: GoNewtypeCarrier,
    },
    Transparent {
        payload: GoFacadeUseRef<'site, 'source>,
    },
    Product(GoProductUse<'site, 'source>),
    Sum(GoSumUse<'site, 'source>),
    Function(GoFunctionUse<'site, 'source>),
    Forall(GoForallUse<'site, 'source>),
}

/// One contextual product field.
pub(crate) struct GoProductFieldUse<'site, 'source> {
    name: GoIdentifier,
    payload: GoFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> GoProductFieldUse<'site, 'source> {
    pub(crate) fn name(&self) -> &GoIdentifier {
        &self.name
    }

    pub(crate) fn payload(&self) -> &GoFacadeUseRef<'site, 'source> {
        &self.payload
    }
}

/// Contextual product type and ordered public fields.
pub(crate) struct GoProductUse<'site, 'source> {
    boundary_type: GoType,
    fields: Vec<GoProductFieldUse<'site, 'source>>,
}

impl<'site, 'source> GoProductUse<'site, 'source> {
    pub(crate) fn boundary_type(&self) -> &GoType {
        &self.boundary_type
    }

    pub(crate) fn fields(&self) -> &[GoProductFieldUse<'site, 'source>] {
        &self.fields
    }
}

/// One contextual sum arm. Case/constructor helpers are global per semantic
/// key and accept a caller-provided exact Row type.
pub(crate) struct GoSumArmUse<'site, 'source> {
    index: u64,
    payload: GoFacadeUseRef<'site, 'source>,
    payload_type: GoType,
    helper: GoSumKey,
    value_accessor: GoIdentifier,
}

impl<'site, 'source> GoSumArmUse<'site, 'source> {
    pub(crate) fn index(&self) -> u64 {
        self.index
    }

    pub(crate) fn payload(&self) -> &GoFacadeUseRef<'site, 'source> {
        &self.payload
    }

    pub(crate) fn payload_type(&self) -> &GoType {
        &self.payload_type
    }

    pub(crate) fn value_accessor(&self) -> &GoIdentifier {
        &self.value_accessor
    }

    pub(crate) fn case_type(&self, row_type: &GoType) -> GoType {
        GoType::new(format!(
            "{}[{}, {}]",
            self.helper.public_case, row_type, self.payload_type
        ))
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn constructor(&self) -> &GoIdentifier {
        &self.helper.constructor
    }

    pub(crate) fn render_constructor(&self, row_type: &GoType, value: &str) -> String {
        format!("{}[{row_type}]({value})", self.helper.constructor)
    }
}

/// Contextual concrete sum alias, its one short Row alias, and ordered arms.
pub(crate) struct GoSumUse<'site, 'source> {
    boundary_type: GoType,
    row_name: GoIdentifier,
    row_alias: GoType,
    arms: Vec<GoSumArmUse<'site, 'source>>,
}

impl<'site, 'source> GoSumUse<'site, 'source> {
    pub(crate) fn boundary_type(&self) -> &GoType {
        &self.boundary_type
    }

    pub(crate) fn row_alias(&self) -> &GoType {
        &self.row_alias
    }

    pub(crate) fn arms(&self) -> &[GoSumArmUse<'site, 'source>] {
        &self.arms
    }

    /// Render one caller-named stable alias and its positional arm API.
    ///
    /// The caller owns the `Env_`/`Exp_` base-name grammar. The facade owns
    /// every derived case/constructor spelling and uses the contextual short
    /// Row alias, so no arm repeats the full O(N) Row instantiation.
    pub(crate) fn exact_alias(
        &self,
        alias: &GoIdentifier,
        type_parameter_declaration: &str,
        type_arguments: &str,
        removed_at_version: Option<u32>,
    ) -> Result<GoExactSumAlias, GoFacadeError> {
        let mut declaration = String::new();
        render_deprecation_comment(&mut declaration, removed_at_version);
        writeln!(
            declaration,
            "type {alias}{type_parameter_declaration} = {}",
            self.boundary_type
        )
        .expect("writing to String cannot fail");
        let mut claims = vec![(
            GoFacadeScope::Package,
            alias.clone(),
            GoSumAliasNameOwner::Alias {
                row: self.row_name.clone(),
                alias: alias.clone(),
            },
        )];
        for arm in &self.arms {
            let case_alias = go_identifier(format!("{alias}_{}", arm.index))?;
            let constructor = go_identifier(format!("New{alias}_{}", arm.index))?;
            let case_type = arm.case_type(&self.row_alias);
            render_deprecation_comment(&mut declaration, removed_at_version);
            writeln!(
                declaration,
                "type {case_alias}{type_parameter_declaration} = {case_type}"
            )
            .expect("writing to String cannot fail");
            render_deprecation_comment(&mut declaration, removed_at_version);
            writeln!(
                declaration,
                "func {constructor}{type_parameter_declaration}(value {}) {alias}{type_arguments} {{",
                arm.payload_type
            )
            .expect("writing to String cannot fail");
            writeln!(
                declaration,
                "\treturn {}",
                arm.render_constructor(&self.row_alias, "value")
            )
            .expect("writing to String cannot fail");
            writeln!(declaration, "}}").expect("writing to String cannot fail");
            claims.push((
                GoFacadeScope::Package,
                case_alias.clone(),
                GoSumAliasNameOwner::Case {
                    row: self.row_name.clone(),
                    alias: alias.clone(),
                    index: arm.index,
                },
            ));
            claims.push((
                GoFacadeScope::Package,
                constructor.clone(),
                GoSumAliasNameOwner::Constructor {
                    row: self.row_name.clone(),
                    alias: alias.clone(),
                    index: arm.index,
                },
            ));
        }
        declaration.push('\n');
        let claims = ScopeClaims::try_collect(claims).map_err(|collision| {
            GoFacadeError::new(format!(
                "Go sum alias collision at name {} between {:?} and {:?}",
                collision.name(),
                collision.first_owner(),
                collision.second_owner()
            ))
        })?;
        Ok(GoExactSumAlias {
            claims,
            declaration,
        })
    }
}

/// Owner role for names derived from one caller-supplied stable alias base.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoSumAliasNameOwner {
    Alias {
        row: GoIdentifier,
        alias: GoIdentifier,
    },
    Case {
        row: GoIdentifier,
        alias: GoIdentifier,
        index: u64,
    },
    Constructor {
        row: GoIdentifier,
        alias: GoIdentifier,
        index: u64,
    },
}

pub(crate) type GoSumAliasClaims = ScopeClaims<GoFacadeScope, GoIdentifier, GoSumAliasNameOwner>;

/// Fully rendered stable sum-alias surface. Its claims must be folded into
/// the package compositor's one complete validation transaction.
pub(crate) struct GoExactSumAlias {
    claims: GoSumAliasClaims,
    declaration: String,
}

impl GoExactSumAlias {
    pub(crate) fn claims(&self) -> &GoSumAliasClaims {
        &self.claims
    }

    pub(crate) fn declaration(&self) -> &str {
        &self.declaration
    }
}

/// Contextual function type. Slots/result retain their exact source cursors.
pub(crate) struct GoFunctionUse<'site, 'source> {
    source: GoFacadeUseSource,
    boundary_type: GoType,
    slots: Vec<GoFacadeUseRef<'site, 'source>>,
    result: GoFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> GoFunctionUse<'site, 'source> {
    pub(crate) fn source(&self) -> &GoFacadeUseSource {
        &self.source
    }

    pub(crate) fn boundary_type(&self) -> &GoType {
        &self.boundary_type
    }

    pub(crate) fn slots(&self) -> &[GoFacadeUseRef<'site, 'source>] {
        &self.slots
    }

    pub(crate) fn result(&self) -> &GoFacadeUseRef<'site, 'source> {
        &self.result
    }
}

/// One erased type stage and its contextual result.
pub(crate) struct GoForallUse<'site, 'source> {
    source: GoFacadeUseSource,
    binder: FacadeBinderId,
    binder_metadata: &'site FacadeBinder,
    result: GoFacadeUseRef<'site, 'source>,
}

impl<'site, 'source> GoForallUse<'site, 'source> {
    pub(crate) fn source(&self) -> &GoFacadeUseSource {
        &self.source
    }

    pub(crate) fn binder(&self) -> FacadeBinderId {
        self.binder
    }

    pub(crate) fn binder_metadata(&self) -> &FacadeBinder {
        self.binder_metadata
    }

    pub(crate) fn result(&self) -> &GoFacadeUseRef<'site, 'source> {
        &self.result
    }
}

impl<'a> GoFacadeSite<'a> {
    fn source_of(context: &GoFacadeContext) -> Option<GoFacadeUseSource> {
        match context.term.as_ref() {
            GoFacadeTerm::Use { source, .. } => Some(source.clone()),
            GoFacadeTerm::Erased(_) => None,
        }
    }

    fn complete_type(
        &self,
        traversal: &GoFacadeTraversal,
    ) -> Result<GoRenderedType, GoFacadeError> {
        let mut ignored = GoReachability::default();
        match self.render_context(traversal, &mut ignored)? {
            GoRenderResult::Complete(complete) => Ok(complete),
            GoRenderResult::Cycle(name) => {
                Ok(GoRenderedType::erased(GoErasureReason::RecursiveBoth(name)))
            }
        }
    }

    fn view_context<'site>(
        &'site self,
        traversal: &GoFacadeTraversal,
    ) -> Result<GoFacadeUseView<'site, 'a>, GoFacadeError> {
        let context = &traversal.context;
        if let GoFacadeTerm::Erased(reason) = context.term.as_ref() {
            return Ok(GoFacadeUseView::Erased {
                reason: reason.clone(),
            });
        }
        let source =
            Self::source_of(context).expect("a non-erased facade context retains one source use");
        match self.use_at(context)? {
            FacadeUse::Unit { .. } => Ok(GoFacadeUseView::Unit),
            FacadeUse::Bottom { .. } => Ok(GoFacadeUseView::Bottom),
            FacadeUse::Bound { binder, .. } => {
                let substituted = self.resolve_bound_traversal(traversal, *binder)?;
                self.view_context(&substituted)
            }
            FacadeUse::Nominal { name, .. } => self.view_nominal(name, &[], traversal),
            FacadeUse::Apply { .. } => {
                match self.resolve_application_head(traversal, Vec::new())? {
                    GoResolvedHead::Nominal {
                        name,
                        arguments,
                        caller,
                    } => self.view_nominal(&name, &arguments, &caller),
                    GoResolvedHead::Erased(reason) => Ok(GoFacadeUseView::Erased { reason }),
                }
            }
            FacadeUse::Product { shell, args, .. } => {
                let rendered = self.complete_type(traversal)?;
                if let Some(GoErasureReason::RecursiveBoth(name)) = rendered.erasure {
                    return Ok(GoFacadeUseView::Erased {
                        reason: GoErasureReason::RecursiveBoth(name),
                    });
                }
                let shell = GoFacadeShell::new(shell)?;
                let fields = shell
                    .fields()
                    .iter()
                    .cloned()
                    .zip(args)
                    .map(|(name, use_id)| {
                        Ok(GoProductFieldUse {
                            name,
                            payload: GoFacadeUseRef {
                                site: self,
                                traversal: self.child_traversal(traversal, *use_id)?,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>, GoFacadeError>>()?;
                Ok(GoFacadeUseView::Product(GoProductUse {
                    boundary_type: rendered.ty,
                    fields,
                }))
            }
            FacadeUse::Sum { shell, args, .. } => {
                let rendered = self.complete_type(traversal)?;
                if let Some(GoErasureReason::RecursiveBoth(name)) = rendered.erasure {
                    return Ok(GoFacadeUseView::Erased {
                        reason: GoErasureReason::RecursiveBoth(name),
                    });
                }
                let row = self.contextual_sum_rows.get(context).ok_or_else(|| {
                    GoFacadeError::new(
                        "contextual sum use was not included in prepared reachability",
                    )
                })?;
                let shell = GoFacadeShell::new(shell)?;
                let mut arms = Vec::with_capacity(args.len());
                for (index, (key, use_id)) in shell.id().ordered_keys().iter().zip(args).enumerate()
                {
                    let payload_traversal = self.child_traversal(traversal, *use_id)?;
                    let payload_type = self.complete_type(&payload_traversal)?.ty;
                    arms.push(GoSumArmUse {
                        index: u64::try_from(index)
                            .map_err(|_| GoFacadeError::new("sum arm index does not fit in u64"))?,
                        payload: GoFacadeUseRef {
                            site: self,
                            traversal: payload_traversal,
                        },
                        payload_type,
                        helper: GoSumKey::new(key)?,
                        value_accessor: go_identifier(SUM_VALUE_ACCESSOR.to_owned())?,
                    });
                }
                Ok(GoFacadeUseView::Sum(GoSumUse {
                    boundary_type: rendered.ty,
                    row_name: row.name.clone(),
                    row_alias: GoType::new(format!("{}{}", row.name, row.type_arguments)),
                    arms,
                }))
            }
            FacadeUse::Function { slots, result, .. } => {
                let boundary_type = self.complete_type(traversal)?.ty;
                let slots = slots
                    .iter()
                    .map(|use_id| {
                        Ok(GoFacadeUseRef {
                            site: self,
                            traversal: self.child_traversal(traversal, *use_id)?,
                        })
                    })
                    .collect::<Result<Vec<_>, GoFacadeError>>()?;
                let result = GoFacadeUseRef {
                    site: self,
                    traversal: self.child_traversal(traversal, *result)?,
                };
                Ok(GoFacadeUseView::Function(GoFunctionUse {
                    source,
                    boundary_type,
                    slots,
                    result,
                }))
            }
            FacadeUse::Forall { binder, result, .. } => {
                let GoFacadeTerm::Use { substitutions, .. } = context.term.as_ref() else {
                    unreachable!()
                };
                let mut nested = substitutions.as_ref().clone();
                let erased = GoFacadeContext::erased(GoErasureReason::UnboundType);
                nested.insert(*binder, erased);
                let mut bound_args = traversal.bound_args.as_ref().clone();
                bound_args.insert(
                    *binder,
                    GoFacadeTraversal::erased(
                        GoErasureReason::UnboundType,
                        Arc::clone(&traversal.active_newtypes),
                    ),
                );
                let result = GoFacadeUseRef {
                    site: self,
                    traversal: GoFacadeTraversal {
                        context: GoFacadeContext::use_in(
                            source.arena.clone(),
                            *result,
                            Arc::new(nested),
                        ),
                        bound_args: Arc::new(bound_args),
                        active_newtypes: Arc::clone(&traversal.active_newtypes),
                    },
                };
                let binder_metadata = self.arena_plan(&source.arena)?.binder(*binder);
                Ok(GoFacadeUseView::Forall(GoForallUse {
                    source,
                    binder: *binder,
                    binder_metadata,
                    result,
                }))
            }
        }
    }

    fn view_nominal<'site>(
        &'site self,
        name: &QualifiedTypeName,
        arguments: &[GoFacadeTraversal],
        caller: &GoFacadeTraversal,
    ) -> Result<GoFacadeUseView<'site, 'a>, GoFacadeError> {
        let nominal = self.nominals.get(name).ok_or_else(|| {
            GoFacadeError::new(format!(
                "facade site is missing nominal {}.{}",
                name.module_segments().join("."),
                name.name()
            ))
        })?;
        if arguments.len() != nominal.arity() {
            return Err(GoFacadeError::new(format!(
                "nominal {}.{} expected {} arguments, got {}",
                name.module_segments().join("."),
                name.name(),
                nominal.arity(),
                arguments.len()
            )));
        }
        match nominal {
            GoNominalPlan::Host {
                binding: BoundaryHostTypeBinding::Role(role),
                exact_boundary_type,
                role_adapter,
                ..
            } => Ok(GoFacadeUseView::ExactHost {
                boundary_type: exact_boundary_type
                    .clone()
                    .unwrap_or_else(|| GoType::new(go_role_type(*role))),
                role_adapter: role_adapter.clone(),
            }),
            GoNominalPlan::Host {
                binding: BoundaryHostTypeBinding::Roleless,
                exact_boundary_type: Some(boundary_type),
                ..
            } => Ok(GoFacadeUseView::ExactHost {
                boundary_type: boundary_type.clone(),
                role_adapter: None,
            }),
            GoNominalPlan::Host {
                parametric_carrier: Some(carrier),
                ..
            } => {
                let mut ignored = GoReachability::default();
                let rendered = match self.render_nominal(name, arguments, caller, &mut ignored)? {
                    GoRenderResult::Complete(rendered) => rendered.ty,
                    GoRenderResult::Cycle(target) => {
                        return Ok(GoFacadeUseView::Erased {
                            reason: GoErasureReason::RecursiveBoth(target),
                        });
                    }
                };
                Ok(GoFacadeUseView::HostCarrier {
                    boundary_type: rendered,
                    carrier: carrier.clone(),
                })
            }
            GoNominalPlan::Host {
                binding: BoundaryHostTypeBinding::Roleless,
                exact_boundary_type: None,
                ..
            } => Ok(GoFacadeUseView::Erased {
                reason: GoErasureReason::RolelessHost(name.clone()),
            }),
            GoNominalPlan::Carrier { carrier, .. } => Ok(GoFacadeUseView::Carrier {
                carrier: carrier.clone(),
            }),
            GoNominalPlan::Transparent { .. } => {
                let mut ignored = GoReachability::default();
                let rendered = match self.render_nominal(name, arguments, caller, &mut ignored)? {
                    GoRenderResult::Complete(complete) => complete,
                    GoRenderResult::Cycle(target) => {
                        GoRenderedType::erased(GoErasureReason::RecursiveBoth(target))
                    }
                };
                if let Some(GoErasureReason::RecursiveBoth(target)) = rendered.erasure {
                    return Ok(GoFacadeUseView::Erased {
                        reason: GoErasureReason::RecursiveBoth(target),
                    });
                }
                let payload = GoFacadeUseRef {
                    site: self,
                    traversal: self.instantiate_payload_traversal(name, arguments, caller)?,
                };
                Ok(GoFacadeUseView::Transparent { payload })
            }
        }
    }
}

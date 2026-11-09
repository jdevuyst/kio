//! Bodyless declarations checked with the ordinary resolver and signature rules.
//!
//! A checking package retains private headers and validated written imports;
//! it is not the minimal exported interface. The shared signature projection
//! subsequently selects public declarations and their exact nominal references.
//! Recording derives the imports for that public text from each defining origin.

use std::marker::PhantomData;

use crate::ast::{Equiv, ExpressionOccurrence, Never, Phase, Prime, Purity, Type, UserMacroDef};
use crate::error::Error;
use crate::pass::resolve::ResolvePhase;
use crate::pass::typecheck_core::declarations::{AuxiliaryItemChecks, ModuleDeferralCheck};
use crate::pass::typecheck_core::{DeclarationPhase, ModuleEnv, NoHeaderRecording};

/// Declaration profile retaining its input phase's signature holes and policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Contract<P = Prime>(PhantomData<P>);

/// A contract function has a signature, but no executable expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NoBody;

/// Bodyless contracts retain compile-time imports as declaration-resolution
/// metadata; they do not provide an executable Prime capability environment.
pub(crate) fn check_imports(imports: &[crate::ast::Import]) -> Result<(), Error> {
    for import in imports {
        if !matches!(import.kind, crate::ast::ImportKind::Comptime) {
            crate::prime::lower::check_imports(std::slice::from_ref(import))?;
        }
    }
    Ok(())
}

/// Lower only the declared headers of an already-selected source module.
/// The declaration walkers are the ordinary frontend walkers; they never
/// receive a function definition or its discarded body.
#[cfg(any(feature = "surface", feature = "prime"))]
pub(crate) fn lower_module<P>(
    module: crate::ast::Module<crate::ast::Surface>,
    mut lower_header: impl FnMut(
        crate::ast::Signature<crate::ast::Surface>,
        Type<crate::ast::Surface>,
    ) -> Result<(crate::ast::Signature<P>, Type<P>), Error>,
    mut lower_declaration: impl FnMut(
        crate::ast::Item<crate::ast::Surface>,
    ) -> Result<Vec<crate::ast::Item<P>>, Error>,
) -> Result<crate::ast::Module<Contract<P>>, Error>
where
    P: ResolvePhase + std::fmt::Debug + Eq,
    P::FnDefRetElided: crate::ast::PhaseBridge<bool>,
{
    use crate::ast::{FnDef, Item, Meta, PhaseBridge};
    let mut items = Vec::with_capacity(module.items.len());
    for item in module.items {
        if let Item::FnDef(def) = item {
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
            } = def;
            let (sig, ret) = lower_header(sig, ret)?;
            items.push(Item::FnDef(FnDef {
                vis,
                purity,
                name,
                sig: crate::ast::convert_signature(&sig),
                ret: crate::ast::convert_type(&ret),
                ret_elided: P::FnDefRetElided::bridge(ret_elided),
                body: NoBody,
                meta: Meta::new(meta.span),
                doc,
            }));
        } else {
            let declarations = match item {
                Item::Labels(owner, _) => {
                    let members = lower_nominal_members(
                        compact_nominal_members(&owner),
                        &mut lower_declaration,
                    )?;
                    if owner.rec_span.is_none() {
                        crate::pass::nominal_declarations::normalize_compact_declaration(
                            &owner, members,
                        )?
                    } else {
                        partition_nominal_members(members, owner.rec_span, owner.meta.span)?
                    }
                }
                Item::TypeRecGroup(group)
                    if group
                        .members
                        .iter()
                        .any(|member| matches!(member, crate::ast::TypeRecMember::Labels(..))) =>
                {
                    let members = lower_nominal_members(group.members, &mut lower_declaration)?;
                    partition_nominal_members(members, group.rec_span, group.meta.span)?
                }
                item => {
                    assert!(
                        matches!(
                            item,
                            Item::HostType(_)
                                | Item::HostFn(_)
                                | Item::TypeAlias(_)
                                | Item::Newtype(_)
                                | Item::TypeRecGroup(_)
                        ),
                        "contract selection leaves only function headers and type/host declarations"
                    );
                    lower_declaration(item)?
                }
            };
            for declaration in declarations {
                items.push(match declaration {
                    Item::HostType(d) => Item::HostType(crate::ast::convert_host_type(&d)),
                    Item::HostFn(d) => Item::HostFn(crate::ast::convert_host_fn(&d)),
                    Item::TypeAlias(d) => Item::TypeAlias(crate::ast::convert_type_alias(&d)),
                    Item::Newtype(d) => Item::Newtype(crate::ast::convert_newtype(&d)),
                    Item::TypeRecGroup(d) => {
                        Item::TypeRecGroup(crate::ast::convert_type_rec_group(&d))
                    }
                    _ => unreachable!(
                        "lowering selected type/host declarations preserves declaration categories"
                    ),
                });
            }
        }
    }
    Ok(crate::ast::Module {
        path: module.path,
        imports: module.imports,
        items,
        meta: Meta::new(module.meta.span),
        doc: module.doc,
    })
}

#[cfg(any(feature = "surface", feature = "prime"))]
fn compact_nominal_members(
    owner: &crate::ast::Labels<crate::ast::Surface>,
) -> Vec<crate::ast::TypeRecMember<crate::ast::Surface>> {
    use crate::ast::{Meta, TypeAlias, TypeRecMember};
    use crate::pass::nominal_declarations::{
        label_nominal_declaration, label_source_owner_span, named_nominal_alias_body,
    };
    let mut members = owner
        .entries
        .iter()
        .map(|entry| {
            TypeRecMember::Newtype(label_nominal_declaration(
                owner,
                entry,
                entry.name.clone(),
                entry.payload.clone(),
            ))
        })
        .collect::<Vec<_>>();
    if let Some(name) = &owner.type_alias_name {
        members.push(TypeRecMember::TypeAlias(TypeAlias {
            vis: owner.vis.clone(),
            name: name.clone(),
            name_span: owner.type_alias_span.expect("named owner has a name span"),
            type_params: owner.type_alias_params.clone(),
            body: named_nominal_alias_body(owner, |entry| vec![entry.name.clone()]),
            meta: Meta::new(label_source_owner_span(owner)),
            editable_span: owner.editable_span,
            doc: None,
        }));
    }
    members
}

/// Expand compact members at the same public projection boundary as explicit
/// nominals. Erased payloads never reach the ordinary header lowerer.
#[cfg(any(feature = "surface", feature = "prime"))]
fn lower_nominal_members<P: ResolvePhase>(
    members: Vec<crate::ast::TypeRecMember<crate::ast::Surface>>,
    lower_declaration: &mut impl FnMut(
        crate::ast::Item<crate::ast::Surface>,
    ) -> Result<Vec<crate::ast::Item<P>>, Error>,
) -> Result<Vec<crate::ast::TypeRecMember<P>>, Error> {
    use crate::ast::{Item, TypeRecMember};
    let mut lowered = Vec::new();
    for member in members {
        let expanded = match member {
            TypeRecMember::Labels(owner, _) => compact_nominal_members(&owner),
            member => vec![member],
        };
        for member in expanded {
            let item = match member {
                TypeRecMember::TypeAlias(alias) => Item::TypeAlias(alias),
                TypeRecMember::Newtype(nominal) => Item::Newtype(if nominal.is_host_exported() {
                    crate::sig::project_newtype_declaration(nominal)
                } else {
                    nominal
                }),
                TypeRecMember::Labels(..) => {
                    unreachable!("compact expansion returns ordinary members")
                }
            };
            for item in lower_declaration(item)? {
                lowered.push(match item {
                    Item::TypeAlias(alias) => TypeRecMember::TypeAlias(alias),
                    Item::Newtype(nominal) => TypeRecMember::Newtype(nominal),
                    _ => unreachable!("ordinary type header lowering preserves its category"),
                });
            }
        }
    }
    Ok(lowered)
}

/// Contract projection partitions the surviving graph after opaque erasure,
/// using the same policy as explicit recursive declarations.
#[cfg(any(feature = "surface", feature = "prime"))]
fn partition_nominal_members<P: ResolvePhase>(
    members: Vec<crate::ast::TypeRecMember<P>>,
    rec_span: Option<crate::span::Span>,
    span: crate::span::Span,
) -> Result<Vec<crate::ast::Item<P>>, Error> {
    let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
    if let Some(error) = crate::pass::nominal_declarations::alias_cycle_error(&members, &analysis) {
        return Err(error);
    }
    Ok(crate::pass::resolve::emit_type_rec_partition(
        crate::ast::TypeRecGroup {
            members,
            doc: None,
            source_layout: None,
            rec_span,
            open_brace_span: None,
            close_brace_span: None,
            deferred_rec_labels_diagnostic: None,
            meta: crate::ast::Meta::new(span),
        },
        &analysis,
    ))
}

#[cfg(feature = "cli")]
pub(crate) fn package_file<P: Phase>(
    file: crate::ast::PackageFile<crate::ast::Surface>,
) -> crate::ast::PackageFile<P> {
    crate::ast::PackageFile {
        name: file.name,
        build: file.build,
        bridge: file.bridge,
        meta: crate::ast::Meta::new(file.meta.span),
    }
}

impl<P: Phase + Clone + std::fmt::Debug + Eq> Phase for Contract<P> {
    type FnBody = NoBody;
    type ExprBlockSyntax = Never;
    type ExpressionOccurrence = ExpressionOccurrence;
    type CallDiagnostic = ();
    type LitAnnotation = Option<Type<Self>>;
    type ExprTuple = Never;
    type ExprFnPlaceholder = Never;
    type ExprOpChain = Never;
    type ExprElab = Never;
    type ExprRecCall = Never;
    type ExprRecOrder = Never;
    type ExprUfcs = Never;
    type ExprEnriched = Never;
    type ExprEnrichedSynthTy = Never;
    type ExprLow = Never;
    type ExprResolved = ();
    type TypeInfer = P::TypeInfer;
    type TypeGoal = P::TypeGoal;
    type ItemLabels = Never;
    type ItemEquiv = Never;
    type ItemMacro = Never;
    type ItemOp = Never;
    type ItemRecGroup = Never;
    type ItemLiteralAlias = Never;
    type ParamPatternExt = ();
    type FnDefRetElided = P::FnDefRetElided;
    type FnPurity = Purity;
    type LeadingTrivia = ();
    type SourceDelimitedTrivia = ();
    type SourceSignatureTrivia = ();
    type FnExprCapabilities = ();
    type FnTypeCapabilities = ();
}

impl<P: Phase + Clone + std::fmt::Debug + Eq> ResolvePhase for Contract<P> {}
impl<P: Phase + Clone + std::fmt::Debug + Eq> crate::pass::visit_mut::TypecheckVisitPhase
    for Contract<P>
{
}

#[cfg(feature = "surface")]
impl DeclarationPhase for Contract<crate::ast::Lowered> {
    type Elaborations = ();
    type HeaderRecording = NoHeaderRecording;
    type AuxiliaryItems = Self;

    fn merge_elaborations(_: &mut (), _: ()) {}
}

impl DeclarationPhase for Contract<Prime> {
    type Elaborations = ();
    type HeaderRecording = NoHeaderRecording;
    type AuxiliaryItems = Self;

    fn merge_elaborations(_: &mut (), _: ()) {}
}

impl<P: Phase + Clone + std::fmt::Debug + Eq> AuxiliaryItemChecks<Contract<P>> for Contract<P>
where
    Self: DeclarationPhase<Elaborations = ()> + Phase<ItemEquiv = Never, ItemMacro = Never>,
{
    fn check_equiv<'m>(
        _: &'m Equiv<Self>,
        witness: &Never,
        _: usize,
        _: &'m ModuleEnv<'m, Self>,
        _: &mut (),
    ) -> Result<(), Error> {
        match *witness {}
    }

    fn check_macro<'m>(
        _: &'m UserMacroDef<Self>,
        witness: &Never,
        _: usize,
        _: &'m ModuleEnv<'m, Self>,
        _: &mut (),
    ) -> Result<(), Error> {
        match *witness {}
    }

    const DISCHARGE: Option<ModuleDeferralCheck<Self>> = None;
}

#[cfg(test)]
mod tests {
    #[test]
    fn contract_imports_preserve_metadata_without_relaxing_prime_modules() {
        let comptime = crate::pass::parser::parse("module x; import __comptime__;")
            .expect("parse compile-time import")
            .imports;
        super::check_imports(&comptime).expect("bodyless contract retains the type environment");
        crate::prime::lower::check_imports(&comptime)
            .expect_err("executable Prime cannot import compile-time capabilities");

        let intrinsics = crate::pass::parser::parse("module x; import __intrinsics__;")
            .expect("parse intrinsic import")
            .imports;
        super::check_imports(&intrinsics).expect("contract intrinsic import");
        crate::prime::lower::check_imports(&intrinsics).expect("Prime intrinsic import");

        for source in [
            "module x; import provider(macro tag);",
            "module x; import provider(op _ + _);",
        ] {
            let imports = crate::pass::parser::parse(source)
                .expect("parse surface selector")
                .imports;
            let contract = super::check_imports(&imports)
                .expect_err("bodyless contracts reject surface selectors");
            let prime = crate::prime::lower::check_imports(&imports)
                .expect_err("Prime rejects surface selectors");
            assert_eq!(format!("{contract:?}"), format!("{prime:?}"));
        }
    }
}

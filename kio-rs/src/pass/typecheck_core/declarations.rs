use crate::ast::{Equiv, ExpressionOccurrence, FnDef, Never, Phase, UserElaboratorDef};
use crate::error::Error;
use crate::pass::resolve::ResolvePhase;
use crate::span::Span;

use super::{ModuleEnv, PackageTypecheckScope, TypeCtx, Typer, TyperPhase};

/// Declaration checking does not require an expression dispatcher or a body.
pub trait DeclarationPhase:
    Phase<ExprResolved = (), ExpressionOccurrence = ExpressionOccurrence> + ResolvePhase
{
    type Elaborations: Default;
    type HeaderRecording: HeaderRecording<Self>;
    type AuxiliaryItems: AuxiliaryItemChecks<Self>;

    fn new_elaborations(_scope: &PackageTypecheckScope<Self>) -> Self::Elaborations {
        Self::Elaborations::default()
    }

    fn merge_elaborations(target: &mut Self::Elaborations, source: Self::Elaborations);

    const CAPABILITY_SIGNATURE: Option<CapabilitySignatureCheck<Self>> = None;
}

pub type CapabilitySignatureCheck<P> =
    for<'m, 'e> fn(&'m FnDef<P>, &TypeCtx<'m, 'e, P>) -> Result<(), Error>;

/// Optional editor publication shared by declaration and expression checking.
pub trait HeaderRecording<P: DeclarationPhase> {
    const RECORD_NOMINAL_TYPE_BINDERS: bool;

    fn record_binder_at<'m>(
        segment_span: Span,
        kind: super::ResolvedBinderKind,
        name: &str,
        parent: Option<&str>,
        qualifier: Option<&str>,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    fn record_nominal_type_binder_at<'m>(
        segment_span: Span,
        kind: super::NominalTypeBinderKind,
        declaring_module_path: &str,
        name: &str,
        tcx: &mut TypeCtx<'m, '_, P>,
    );

    fn record_binder_decl_at<'m>(
        decl_span: Span,
        kind: super::ResolvedBinderKind,
        name: &str,
        tcx: &mut TypeCtx<'m, '_, P>,
    );
}

pub struct NoHeaderRecording;

impl<P: DeclarationPhase> HeaderRecording<P> for NoHeaderRecording {
    const RECORD_NOMINAL_TYPE_BINDERS: bool = false;

    fn record_binder_at<'m>(
        _: Span,
        _: super::ResolvedBinderKind,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: &mut TypeCtx<'m, '_, P>,
    ) {
    }

    fn record_nominal_type_binder_at<'m>(
        _: Span,
        _: super::NominalTypeBinderKind,
        _: &str,
        _: &str,
        _: &mut TypeCtx<'m, '_, P>,
    ) {
    }

    fn record_binder_decl_at<'m>(
        _: Span,
        _: super::ResolvedBinderKind,
        _: &str,
        _: &mut TypeCtx<'m, '_, P>,
    ) {
    }
}

pub type ModuleDeferralCheck<P> =
    for<'m> fn(&ModuleEnv<'m, P>, &mut <P as DeclarationPhase>::Elaborations) -> Result<(), Error>;

pub trait AuxiliaryItemChecks<P: DeclarationPhase> {
    fn check_equiv<'m>(
        item: &'m Equiv<P>,
        witness: &P::ItemEquiv,
        index: usize,
        env: &'m ModuleEnv<'m, P>,
        elaborations: &mut P::Elaborations,
    ) -> Result<(), Error>
    where
        P: Phase<TypeLabelSugar = Never> + Clone;

    fn check_elaborator<'m>(
        item: &'m UserElaboratorDef<P>,
        witness: &P::ItemElaborator,
        index: usize,
        env: &'m ModuleEnv<'m, P>,
        elaborations: &mut P::Elaborations,
    ) -> Result<(), Error>;

    const DISCHARGE: Option<ModuleDeferralCheck<P>>;
}

pub struct BodyItemChecks;

impl<P: TyperPhase> AuxiliaryItemChecks<P> for BodyItemChecks {
    fn check_equiv<'m>(
        item: &'m Equiv<P>,
        _: &P::ItemEquiv,
        index: usize,
        env: &'m ModuleEnv<'m, P>,
        elaborations: &mut P::Elaborations,
    ) -> Result<(), Error>
    where
        P: Phase<TypeLabelSugar = Never> + Clone,
    {
        super::check_equiv(item, index, env, elaborations)
    }

    fn check_elaborator<'m>(
        item: &'m UserElaboratorDef<P>,
        _: &P::ItemElaborator,
        index: usize,
        env: &'m ModuleEnv<'m, P>,
        elaborations: &mut P::Elaborations,
    ) -> Result<(), Error> {
        P::Typer::check_user_elaborator_item(item, index, env, elaborations)
    }

    const DISCHARGE: Option<ModuleDeferralCheck<P>> = Some(P::Typer::discharge_module_deferrals);
}

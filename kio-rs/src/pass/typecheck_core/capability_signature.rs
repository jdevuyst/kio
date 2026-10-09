use std::collections::HashSet;

use super::{AliasCtx, DeclarationPhase, TypeCtx};
use crate::ast::{FnDef, FnPurityExt, Type, TypeParam};
use crate::error::Error;
use crate::pass::visit_mut::TypecheckVisitPhase;
use crate::span::Span;

#[derive(Default)]
struct CapabilitySignatureUses {
    bound: Vec<String>,
    handle: Option<(crate::comptime::ComptimeBuiltin, Span)>,
    fill_ctx: Option<Span>,
}

fn direct_capability_identity<P: crate::ast::Phase>(
    ty: &Type<P>,
    is_bound: impl Fn(&str) -> bool,
) -> Option<crate::comptime::ComptimeBuiltin> {
    let Type::Path { segments, args, .. } = ty else {
        return None;
    };
    let [name] = segments.as_slice() else {
        return None;
    };
    (!is_bound(name.as_str()) && args.is_empty())
        .then(|| crate::comptime::ComptimeBuiltin::from_public_name(name.as_str()))
        .flatten()
        .filter(|builtin| builtin.is_type_name())
}

impl<P: crate::pass::visit_mut::TypecheckVisitPhase> crate::pass::visit_mut::TypecheckVisitMut<P>
    for CapabilitySignatureUses
{
    fn visit_type(&mut self, ty: &mut Type<P>) {
        if let Some(builtin) =
            direct_capability_identity(ty, |name| self.bound.iter().any(|bound| bound == name))
        {
            self.handle.get_or_insert((builtin, ty.span()));
            if builtin == crate::comptime::ComptimeBuiltin::FillCtx {
                self.fill_ctx.get_or_insert(ty.span());
            }
        }
        crate::pass::visit_mut::walk_type(self, ty);
    }

    fn enter_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
        self.bound.push(param.name.clone());
    }

    fn exit_type_binder(&mut self, param: &mut TypeParam, _scope: Span) {
        debug_assert_eq!(self.bound.pop().as_deref(), Some(param.name.as_str()));
    }
}

pub(crate) fn check_fn_capability_signature<'m, P>(
    definition: &'m FnDef<P>,
    tcx: &TypeCtx<'m, '_, P>,
) -> Result<(), Error>
where
    P: DeclarationPhase + TypecheckVisitPhase,
{
    if !definition.purity.is_pure_fn() {
        return Ok(());
    }
    let mut bound = HashSet::new();
    let mut direct = [None; 2];
    let mut value_index = 0;
    let mut imports = CapabilitySignatureUses::default();
    let missing_receiver = |span, used: &str, required: &str, position: &str| {
        Error::type_(
            span,
            format!(
                "pure function `{}` uses `{used}`, but its {position} value parameter is not exactly `{required}`",
                definition.name,
            ),
        )
        .with_help(format!("declare a direct `{required}` {position} value parameter"))
    };
    let canonicalize = |ty: &Type<P>, bound: &HashSet<String>| {
        let alias_ctx = AliasCtx {
            binder_locals: Some(bound),
            ..tcx.env.alias_ctx()
        };
        crate::pass::typecheck_core::canonicalize_deep_for_comparison(ty, &alias_ctx, false).0
    };
    for param in &definition.sig.params {
        if value_index == direct.len() {
            break;
        }
        match param {
            crate::ast::SignatureParam::Type(param) => {
                bound.insert(param.name.clone());
            }
            crate::ast::SignatureParam::Value(param) => {
                let canonical = canonicalize(
                    param
                        .ty
                        .as_ref()
                        .expect("named function parameters are annotated"),
                    &bound,
                );
                direct[value_index] =
                    direct_capability_identity(&canonical, |name| bound.contains(name));
                value_index += 1;
            }
        }
    }
    let signature = definition
        .sig
        .signature_ty(definition.ret.clone(), definition.meta.span);
    let mut signature = canonicalize(&signature, &HashSet::new());
    crate::pass::visit_mut::TypecheckVisitMut::visit_type(&mut imports, &mut signature);
    if let Some((handle, span)) = imports.handle
        && direct[0] != Some(crate::comptime::ComptimeBuiltin::ComptimeProof)
    {
        return Err(missing_receiver(
            span,
            handle.public_name(),
            "__Comptime__",
            "first",
        ));
    }
    if let Some(span) = imports.fill_ctx
        && direct[1] != Some(crate::comptime::ComptimeBuiltin::FillCtx)
    {
        return Err(missing_receiver(
            span,
            "__Fill_ctx__",
            "__Fill_ctx__",
            "second",
        ));
    }
    Ok(())
}

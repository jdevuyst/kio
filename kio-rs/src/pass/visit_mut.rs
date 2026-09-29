//! Same-phase mutable traversal for checker-shaped AST boundaries.
//!
//! The walker is generic over the public Lowered/Prime checker inputs and the
//! private PrePrime replay seam. It owns lexical type-binder scope so
//! transformations can observe binder entry and exit without duplicating the
//! AST recursion.

use crate::ast::{
    CallArg, ElaboratorCall, Expr, FnDef, HostFn, HostFnParam, HostType, Item, LitAnnotationExt,
    Lowered, Module, Never, Newtype, Prime, Signature, SignatureParam, Type, TypeAlias, TypeParam,
    UserElaboratorDef,
};
use crate::pass::resolve::Package;
use crate::span::Span;

/// TTG phases with the shared checker-boundary type-bearing shape.
#[doc(hidden)]
pub trait TypecheckVisitPhase:
    crate::ast::Phase<
        ExprTuple = Never,
        ExprFnPlaceholder = Never,
        ExprOpChain = Never,
        ExprLabelValue = Never,
        ExprRowLet = Never,
        ExprBlockSyntax = Never,
        ExprRecCall = Never,
        ExprEnriched = Never,
        ExprEnrichedSynthTy = Never,
        ExprLow = Never,
        TypeLabelSugar = Never,
        ItemLabels = Never,
        ItemOp = Never,
        ItemRecGroup = Never,
        ItemLiteralAlias = Never,
    > + Clone
{
}

impl TypecheckVisitPhase for Lowered {}
impl TypecheckVisitPhase for Prime {}

/// A mutable, same-phase traversal of every type-bearing checker-input node.
pub(crate) trait TypecheckVisitMut<P: TypecheckVisitPhase> {
    fn visit_package(&mut self, package: &mut Package<P>) {
        walk_package(self, package);
    }

    fn visit_module(&mut self, module: &mut Module<P>) {
        walk_module(self, module);
    }

    fn visit_item(&mut self, item: &mut Item<P>) {
        walk_item(self, item);
    }

    fn visit_expr(&mut self, expr: &mut Expr<P>) {
        walk_expr(self, expr);
    }

    fn visit_type(&mut self, ty: &mut Type<P>) {
        walk_type(self, ty);
    }

    /// Called before a lexical type parameter becomes visible.
    fn enter_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {}

    /// Called after the lexical type parameter's scope has been traversed.
    fn exit_type_binder(&mut self, _param: &mut TypeParam, _scope: Span) {}

    /// Called before a lexical value parameter or `let` binder becomes visible.
    fn enter_value_binder(&mut self, _name: &mut String, _span: Span, _scope: Span) {}

    /// Called after a lexical value binder's scope has been traversed.
    fn exit_value_binder(&mut self, _name: &mut String, _span: Span, _scope: Span) {}
}

pub(crate) fn walk_package<P, V>(visitor: &mut V, package: &mut Package<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for entry in package.modules_mut() {
        visitor.visit_module(&mut entry.module);
    }
}

pub(crate) fn walk_module<P, V>(visitor: &mut V, module: &mut Module<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for item in &mut module.items {
        visitor.visit_item(item);
    }
}

pub(crate) fn walk_item<P, V>(visitor: &mut V, item: &mut Item<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    match item {
        Item::FnDef(def) => walk_fn_def(visitor, def),
        Item::TypeAlias(alias) => walk_type_alias(visitor, alias),
        Item::Newtype(newtype) => walk_newtype(visitor, newtype),
        Item::TypeRecGroup(group) => {
            for member in &mut group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => walk_type_alias(visitor, alias),
                    crate::ast::TypeRecMember::Newtype(newtype) => walk_newtype(visitor, newtype),
                    crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                }
            }
        }
        Item::Equiv(equiv, _) => {
            let scope = equiv.meta.span;
            enter_signature(visitor, &mut equiv.sig, scope);
            for term in &mut equiv.terms {
                visitor.visit_expr(&mut term.body);
            }
            exit_signature(visitor, &mut equiv.sig, scope);
        }
        Item::Elaborator(elab, _) => walk_elaborator(visitor, elab),
        Item::HostType(host) => walk_host_type(visitor, host),
        Item::HostFn(host) => walk_host_fn(visitor, host),
        Item::RecGroup(_, ext)
        | Item::LiteralAlias(_, ext)
        | Item::Labels(_, ext)
        | Item::LabelForward(_, ext)
        | Item::Op(_, ext)
        | Item::VariadicOperator(_, ext) => match *ext {},
    }
}

fn walk_fn_def<P, V>(visitor: &mut V, def: &mut FnDef<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    let scope = def.meta.span;
    enter_signature(visitor, &mut def.sig, scope);
    visitor.visit_type(&mut def.ret);
    visitor.visit_expr(&mut def.body);
    exit_signature(visitor, &mut def.sig, scope);
}

fn walk_type_alias<P, V>(visitor: &mut V, alias: &mut TypeAlias<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    let scope = alias.meta.span;
    enter_binders(visitor, &mut alias.type_params, scope);
    visitor.visit_type(&mut alias.body);
    exit_binders(visitor, &mut alias.type_params, scope);
}

fn walk_newtype<P, V>(visitor: &mut V, newtype: &mut Newtype<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    let scope = newtype.meta.span;
    enter_binders(visitor, &mut newtype.type_params, scope);
    enter_binders(visitor, &mut newtype.existential_params, scope);
    visitor.visit_type(&mut newtype.payload);
    exit_binders(visitor, &mut newtype.existential_params, scope);
    exit_binders(visitor, &mut newtype.type_params, scope);
}

fn walk_elaborator<P, V>(visitor: &mut V, elab: &mut UserElaboratorDef<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    visitor.visit_type(&mut elab.call_ty);
}

fn walk_host_type<P, V>(visitor: &mut V, host: &mut HostType<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    let scope = host.meta.span;
    enter_binders(visitor, &mut host.type_params, scope);
    exit_binders(visitor, &mut host.type_params, scope);
}

fn walk_host_fn<P, V>(visitor: &mut V, host: &mut HostFn<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    let scope = host.meta.span;
    for param in &mut host.params {
        match param {
            HostFnParam::Type(param) => visitor.enter_type_binder(param, scope),
            HostFnParam::Value(param) => visitor.visit_type(&mut param.ty),
        }
    }
    visitor.visit_type(&mut host.ret);
    for param in host.params.iter_mut().rev() {
        if let HostFnParam::Type(param) = param {
            visitor.exit_type_binder(param, scope);
        }
    }
}

fn enter_signature<P, V>(visitor: &mut V, sig: &mut Signature<P>, scope: Span)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for param in &mut sig.params {
        match param {
            SignatureParam::Type(param) => visitor.enter_type_binder(param, scope),
            SignatureParam::Value(param) => {
                if let Some(ty) = &mut param.ty {
                    visitor.visit_type(ty);
                }
            }
        }
    }
    for param in &mut sig.params {
        if let SignatureParam::Value(param) = param {
            visitor.enter_value_binder(&mut param.name, param.meta.span, scope);
        }
    }
}

fn exit_signature<P, V>(visitor: &mut V, sig: &mut Signature<P>, scope: Span)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for param in sig.params.iter_mut().rev() {
        if let SignatureParam::Value(param) = param {
            visitor.exit_value_binder(&mut param.name, param.meta.span, scope);
        }
    }
    for param in sig.params.iter_mut().rev() {
        if let SignatureParam::Type(param) = param {
            visitor.exit_type_binder(param, scope);
        }
    }
}

fn enter_binders<P, V>(visitor: &mut V, params: &mut [TypeParam], scope: Span)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for param in params {
        visitor.enter_type_binder(param, scope);
    }
}

fn exit_binders<P, V>(visitor: &mut V, params: &mut [TypeParam], scope: Span)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for param in params.iter_mut().rev() {
        visitor.exit_type_binder(param, scope);
    }
}

pub(crate) fn walk_type<P, V>(visitor: &mut V, ty: &mut Type<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    match ty {
        Type::Path { args, .. } | Type::Goal { args, .. } => {
            for arg in args {
                visitor.visit_type(arg);
            }
        }
        Type::Function { param, ret, .. }
        | Type::Product {
            left: param,
            right: ret,
            ..
        }
        | Type::Sum {
            left: param,
            right: ret,
            ..
        } => {
            visitor.visit_type(param);
            visitor.visit_type(ret);
        }
        Type::Forall { param, body, meta } => {
            let scope = meta.span;
            visitor.enter_type_binder(param, scope);
            visitor.visit_type(body);
            visitor.exit_type_binder(param, scope);
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

pub(crate) fn walk_expr<P, V>(visitor: &mut V, expr: &mut Expr<P>)
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { .. } | Expr::Unit { .. } => {}
        Expr::Call { callee, args, .. } => {
            visitor.visit_expr(callee);
            visit_call_args(visitor, args);
        }
        Expr::FnExpr {
            sig,
            ret_ty,
            body,
            meta,
            ..
        } => {
            let scope = meta.span;
            enter_signature(visitor, sig, scope);
            if let Some(ret_ty) = ret_ty {
                visitor.visit_type(ret_ty);
            }
            visitor.visit_expr(body);
            exit_signature(visitor, sig, scope);
        }
        Expr::Let {
            name,
            name_span,
            ty,
            value,
            body,
            meta,
            ..
        } => {
            if let Some(ty) = ty {
                visitor.visit_type(ty);
            }
            visitor.visit_expr(value);
            visitor.enter_value_binder(name, *name_span, meta.span);
            visitor.visit_expr(body);
            visitor.exit_value_binder(name, *name_span, meta.span);
        }
        Expr::Seq { value, body, .. } => {
            visitor.visit_expr(value);
            visitor.visit_expr(body);
        }
        Expr::StrLit { annotation, .. }
        | Expr::IntLit { annotation, .. }
        | Expr::FloatLit { annotation, .. }
        | Expr::BoolLit { annotation, .. } => {
            if let Some(ty) = annotation.as_type_mut() {
                visitor.visit_type(ty);
            }
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, .. } => visitor.visit_expr(receiver),
            ElaboratorCall::FieldUpdate {
                receiver, updates, ..
            } => {
                visitor.visit_expr(receiver);
                for update in updates {
                    visitor.visit_expr(&mut update.value);
                }
            }
        },
        Expr::RecOrder { plan, .. } => {
            if let Some(continuation) = &mut plan.tail_continuation {
                visitor.visit_expr(continuation);
            }
            if let Some(annotation) = &mut plan.annotation {
                visitor.visit_type(annotation);
            }
            visitor.visit_expr(&mut plan.value);
            let binder_span = plan.value.span();
            let scope = plan.body.span();
            visitor.enter_value_binder(&mut plan.name, binder_span, scope);
            visitor.visit_expr(&mut plan.body);
            visitor.exit_value_binder(&mut plan.name, binder_span, scope);
            visitor.visit_type(&mut plan.runtime_ty);
        }
        Expr::RecQuote { plan, .. } => match plan.as_mut() {
            crate::ast::RecQuotePlan::Operand {
                public_ty,
                runtime_ty,
                computation,
            } => {
                if let Some(public_ty) = public_ty {
                    visitor.visit_type(public_ty);
                }
                visitor.visit_type(runtime_ty);
                visitor.visit_expr(computation);
            }
            crate::ast::RecQuotePlan::Expansion {
                runtime_ty,
                continuation,
                value,
            } => {
                visitor.visit_type(runtime_ty);
                visitor.visit_expr(continuation);
                visitor.visit_expr(value);
            }
        },
        Expr::UserElaborator { args, .. } => visit_call_args(visitor, args),
        Expr::Ufcs { receiver, args, .. } => {
            visitor.visit_expr(receiver);
            visit_call_args(visitor, args);
        }
        Expr::RecCall { ext, .. }
        | Expr::RowLet { ext, .. }
        | Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::OpChain { ext, .. }
        | Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. }
        | Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn visit_call_args<P, V>(visitor: &mut V, args: &mut [CallArg<P>])
where
    P: TypecheckVisitPhase,
    V: TypecheckVisitMut<P> + ?Sized,
{
    for arg in args {
        match arg {
            CallArg::Type(ty) => visitor.visit_type(ty),
            CallArg::Value(expr) => visitor.visit_expr(expr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Meta, TypeGoalRef};

    #[test]
    fn type_visitor_descends_into_goal_arguments() {
        struct Counter(usize);

        impl TypecheckVisitMut<Lowered> for Counter {
            fn visit_type(&mut self, ty: &mut Type<Lowered>) {
                self.0 += 1;
                walk_type(self, ty);
            }
        }

        let span = Span::new(0, 1);
        let mut ty = Type::Goal {
            goal: TypeGoalRef::for_test(0, 1, 2),
            args: vec![Type::synth_path(vec!["A".to_owned()], Vec::new(), span)],
            meta: Meta::new(span),
            ext: (),
        };
        let mut counter = Counter(0);
        counter.visit_type(&mut ty);
        assert_eq!(counter.0, 2);
    }
}

//! Desugared → Lowered walker trait.
//!
//! The [`DesugaredToLowered`] trait pairs every Desugared AST
//! variant with a `visit_*` method that converts it to its Lowered
//! counterpart. Variants inhabited at Lowered have default
//! implementations that clone-and-recurse via `self.walk_*`.
//! Variants uninhabited at Lowered — `Expr::LabelValue`,
//! `Type::LabelSugar`, `Item::Labels` — have no default
//! `rewrite_*` methods the implementer must provide. `Expr::Tuple`
//! / `Expr::FnPlaceholder` are uninhabited already at Desugared
//! (eliminated by [`crate::pass::desugar`]); their dispatch arms in
//! `walk_expr` discharge via `match ext {}` and the walker has no
//! hook for them. `Expr::Ufcs` survives to Lowered and is erased by
//! the typer/substitution boundary.
//!
//! `walk_item` returns `Vec<Item<Lowered>>` so an implementer may
//! drop (`vec![]`) or expand to multiple items. [`crate::pass::label_elab`]
//! expands each `Item::Labels` into one generated `Item::Newtype` per explicit
//! declaration plus the named-form `Item::TypeAlias` when present; reuse
//! markers contribute only alias references.
//!
//! Inputs are taken owned (matching label_elab's existing API).

use crate::ast::{
    CallArg, Desugared, ElaboratorCall, Equiv, Expr, FieldAccessLabel, FieldUpdateLabel, FnDef,
    HostFn, HostFnParam, HostFnValueParam, HostType, Item, LabelSugarLabel, LabelValueLabel,
    Labels, Lowered, Meta, Module, Newtype, NodeId, PackageFile, Param, RecOrderPlan, Signature,
    SignatureParam, Type, TypeAlias, TypeRecGroup, convert_type_member,
};
use crate::error::Error;

/// Walker over `Desugared → Lowered`. Methods named `visit_*` have
/// default clone-and-recurse implementations. Methods named
/// `rewrite_*` have no default — they handle variants that are
/// uninhabited at Lowered.
pub trait DesugaredToLowered {
    // -------------------------------------------------------------
    // Rewrite hooks (no defaults).
    // -------------------------------------------------------------

    fn rewrite_expr_label_value(
        &mut self,
        labels: Vec<LabelValueLabel<Desugared>>,
        meta: Meta<Desugared>,
        ext: NodeId,
    ) -> Result<Expr<Lowered>, Error>;

    fn rewrite_type_label_sugar(
        &mut self,
        labels: Vec<LabelSugarLabel<Desugared>>,
        meta: Meta<Desugared>,
    ) -> Result<Type<Lowered>, Error>;

    /// `Item::Labels` may be dropped, transformed, or expanded.
    /// label_elab expands each `Labels` to generated `Newtype`s plus
    /// an optional named-form `TypeAlias`.
    fn rewrite_item_labels(&mut self, d: Labels<Desugared>) -> Result<Vec<Item<Lowered>>, Error>;

    fn rewrite_item_label_forward(
        &mut self,
        forward: crate::ast::LabelForward<Desugared>,
    ) -> Result<Vec<Item<Lowered>>, Error>;

    fn rewrite_type_rec_group(
        &mut self,
        group: TypeRecGroup<Desugared>,
    ) -> Result<Vec<Item<Lowered>>, Error>;

    fn walk_rec_order_plan(
        &mut self,
        plan: RecOrderPlan<Desugared>,
    ) -> Result<RecOrderPlan<Lowered>, Error> {
        let RecOrderPlan {
            name,
            disposition,
            tail_continuation,
            annotation,
            value,
            body,
            runtime_ty,
        } = plan;
        Ok(RecOrderPlan {
            name,
            disposition,
            tail_continuation: tail_continuation
                .map(|expr| self.walk_expr(*expr).map(Box::new))
                .transpose()?,
            annotation: annotation.map(|ty| self.walk_type(ty)).transpose()?,
            value: Box::new(self.walk_expr(*value)?),
            body: Box::new(self.walk_expr(*body)?),
            runtime_ty: self.walk_type(runtime_ty)?,
        })
    }

    // -------------------------------------------------------------
    // Per-variant visit methods (default-recurse).
    //
    // The walk_* dispatchers route each AST variant to its own visit_*
    // method; the let-else at a method's head can fail only if a
    // dispatcher routed the wrong variant — a bug in this file, hence
    // the bare `unreachable!()`.
    // -------------------------------------------------------------

    fn visit_expr_path(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Path {
            occurrence: _,
            segments,
            meta: Meta { span, .. },
            ext: (),
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Path {
            occurrence: Default::default(),
            segments,
            meta: Meta::new(span),
            ext: (),
        })
    }

    fn visit_expr_call(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Call {
            occurrence: _,
            callee,
            args,
            meta: Meta { span, .. },
            ext: (),
        } = e
        else {
            unreachable!()
        };
        let callee = self.walk_expr(*callee)?;
        let args: Vec<CallArg<Lowered>> = args
            .into_iter()
            .map(|a| self.walk_call_arg(a))
            .collect::<Result<_, _>>()?;
        Ok(Expr::synth_call(callee, args, span))
    }

    fn visit_expr_fn(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta: Meta { span, .. },
            caps: _,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(self.walk_signature_params(sig.params)?, sig.groups),
            ret_ty: ret_ty.map(|t| self.walk_type(t)).transpose()?,
            body: Box::new(self.walk_expr(*body)?),
            meta: Meta::new(span),
            caps: (),
        })
    }

    fn visit_expr_let(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern: (),
            value,
            body,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty: ty.map(|t| self.walk_type(t)).transpose()?,
            pattern: (),
            value: Box::new(self.walk_expr(*value)?),
            body: Box::new(self.walk_expr(*body)?),
            meta: Meta::new(span),
        })
    }

    fn visit_expr_seq(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Seq {
            occurrence: _,
            value,
            body,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(self.walk_expr(*value)?),
            body: Box::new(self.walk_expr(*body)?),
            meta: Meta::new(span),
        })
    }

    fn visit_expr_unit(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Unit {
            occurrence: _,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        })
    }

    fn visit_expr_str_lit(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::StrLit {
            occurrence: _,
            value,
            annotation,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::StrLit {
            occurrence: Default::default(),
            value,
            annotation: annotation.map(|t| self.walk_type(t)).transpose()?,
            meta: Meta::new(span),
        })
    }

    fn visit_expr_int_lit(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::IntLit {
            occurrence: _,
            digits,
            annotation,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::IntLit {
            occurrence: Default::default(),
            digits,
            annotation: annotation.map(|t| self.walk_type(t)).transpose()?,
            meta: Meta::new(span),
        })
    }

    fn visit_expr_float_lit(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::FloatLit {
            occurrence: _,
            digits,
            annotation,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::FloatLit {
            occurrence: Default::default(),
            digits,
            annotation: annotation.map(|t| self.walk_type(t)).transpose()?,
            meta: Meta::new(span),
        })
    }

    fn visit_expr_bool_lit(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::BoolLit {
            occurrence: _,
            value,
            annotation,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::BoolLit {
            occurrence: Default::default(),
            value,
            annotation: annotation.map(|t| self.walk_type(t)).transpose()?,
            meta: Meta::new(span),
        })
    }

    fn visit_expr_elaborator(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Elaborator {
            occurrence: _,
            kind,
            call,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        let call = match call {
            ElaboratorCall::FieldAccess { receiver, labels } => ElaboratorCall::FieldAccess {
                receiver: Box::new(self.walk_expr(*receiver)?),
                labels: labels
                    .into_iter()
                    .map(|label| {
                        let FieldAccessLabel {
                            label,
                            label_span,
                            label_type,
                            meta: Meta { span, .. },
                        } = label;
                        Ok(FieldAccessLabel {
                            label,
                            label_span,
                            label_type,
                            meta: Meta::new(span),
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            },
            ElaboratorCall::FieldUpdate { receiver, updates } => ElaboratorCall::FieldUpdate {
                receiver: Box::new(self.walk_expr(*receiver)?),
                updates: updates
                    .into_iter()
                    .map(|update| {
                        let FieldUpdateLabel {
                            label,
                            label_span,
                            label_type,
                            value,
                            meta: Meta { span, .. },
                        } = update;
                        Ok(FieldUpdateLabel {
                            label,
                            label_span,
                            label_type,
                            value: self.walk_expr(value)?,
                            meta: Meta::new(span),
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            },
        };
        Ok(Expr::Elaborator {
            occurrence: Default::default(),
            kind,
            call,
            meta: Meta::new(span),
            ext,
        })
    }

    fn visit_expr_rec_order(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::RecOrder {
            occurrence: _,
            plan,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(self.walk_rec_order_plan(*plan)?),
            meta: Meta::new(span),
            ext,
        })
    }

    fn visit_expr_user_elaborator(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::UserElaborator {
            occurrence: _,
            name,
            form,
            args,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::UserElaborator {
            occurrence: Default::default(),
            name,
            form,
            args: args
                .into_iter()
                .map(|arg| match arg {
                    crate::ast::CallArg::Value(e) => {
                        self.walk_expr(e).map(crate::ast::CallArg::Value)
                    }
                    crate::ast::CallArg::Type(t) => {
                        self.walk_type(t).map(crate::ast::CallArg::Type)
                    }
                })
                .collect::<Result<_, _>>()?,
            meta: Meta::new(span),
            ext,
        })
    }

    fn visit_expr_ufcs(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Ufcs {
            occurrence: _,
            receiver,
            callee_segments,
            callee_span,
            args,
            flavor,
            bang,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        let receiver = Box::new(self.walk_expr(*receiver)?);
        let args = args
            .into_iter()
            .map(|arg| match arg {
                crate::ast::CallArg::Value(e) => self.walk_expr(e).map(crate::ast::CallArg::Value),
                crate::ast::CallArg::Type(t) => self.walk_type(t).map(crate::ast::CallArg::Type),
            })
            .collect::<Result<_, _>>()?;
        Ok(Expr::Ufcs {
            occurrence: Default::default(),
            receiver,
            callee_segments,
            callee_span,
            args,
            flavor,
            bang,
            meta: Meta::new(span),
            ext,
        })
    }

    fn visit_type_path(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Path {
            segments,
            args,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        let args: Vec<Type<Lowered>> = args
            .into_iter()
            .map(|a| self.walk_type(a))
            .collect::<Result<_, _>>()?;
        Ok(Type::synth_path_segments(segments, args, span))
    }

    fn visit_type_unit(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Unit {
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Unit {
            meta: Meta::new(span),
        })
    }

    fn visit_type_bottom(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Bottom {
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Bottom {
            meta: Meta::new(span),
        })
    }

    fn visit_type_function(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Function {
            param,
            ret,
            meta: Meta { span, .. },
            abi_arity,
            ..
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Function {
            param: Box::new(self.walk_type(*param)?),
            ret: Box::new(self.walk_type(*ret)?),
            meta: Meta::new(span),
            abi_arity,
            caps: (),
        })
    }

    fn visit_type_product(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Product {
            left,
            right,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Product {
            left: Box::new(self.walk_type(*left)?),
            right: Box::new(self.walk_type(*right)?),
            meta: Meta::new(span),
        })
    }

    fn visit_type_sum(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Sum {
            left,
            right,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Sum {
            left: Box::new(self.walk_type(*left)?),
            right: Box::new(self.walk_type(*right)?),
            meta: Meta::new(span),
        })
    }

    fn visit_type_forall(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Forall {
            param,
            body,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Forall {
            param,
            body: Box::new(self.walk_type(*body)?),
            meta: Meta::new(span),
        })
    }

    fn visit_type_infer(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Infer {
            meta: Meta { span, .. },
            ext,
        } = ty
        else {
            unreachable!()
        };
        Ok(Type::Infer {
            meta: Meta::new(span),
            ext,
        })
    }

    // -------------------------------------------------------------
    // Dispatchers and sub-walks (default impls).
    // -------------------------------------------------------------

    fn walk_module(&mut self, module: Module<Desugared>) -> Result<Module<Lowered>, Error> {
        let Module {
            path,
            imports,
            items,
            meta: Meta { span, .. },
            doc,
        } = module;
        let mut new_items: Vec<Item<Lowered>> = Vec::with_capacity(items.len());
        for item in items {
            new_items.extend(self.walk_item(item)?);
        }
        Ok(Module {
            path,
            imports,
            items: new_items,
            meta: Meta::new(span),
            doc,
        })
    }

    fn walk_package_file(
        &mut self,
        package: PackageFile<Desugared>,
    ) -> Result<PackageFile<Lowered>, Error> {
        let PackageFile {
            name,
            build,
            bridge,
            meta: Meta { span, .. },
        } = package;
        Ok(PackageFile {
            name,
            build,
            // `bridge` is phase-independent — carried verbatim.
            bridge,
            meta: Meta::new(span),
        })
    }

    fn walk_item(&mut self, item: Item<Desugared>) -> Result<Vec<Item<Lowered>>, Error> {
        match item {
            Item::FnDef(d) => Ok(vec![Item::FnDef(self.walk_fn_def(d)?)]),
            Item::TypeAlias(a) => Ok(vec![Item::TypeAlias(self.walk_alias(a)?)]),
            Item::Newtype(d) => Ok(vec![Item::Newtype(self.walk_newtype(d)?)]),
            Item::Labels(d, _) => self.rewrite_item_labels(d),
            Item::LabelForward(d, _) => self.rewrite_item_label_forward(d),
            Item::TypeRecGroup(group) => self.rewrite_type_rec_group(group),
            Item::Equiv(e, _) => Ok(vec![Item::Equiv(self.walk_equiv(e)?, ())]),
            Item::Elaborator(s, _) => {
                let crate::ast::UserElaboratorDef {
                    vis,
                    name,
                    name_span,
                    trailing_blocks,
                    captures,
                    call_ty,
                    schedule,
                    implementation,
                    body_trivia,
                    meta: Meta { span, .. },
                    doc,
                } = s;
                Ok(vec![Item::Elaborator(
                    crate::ast::UserElaboratorDef {
                        vis,
                        name,
                        name_span,
                        trailing_blocks: trailing_blocks
                            .into_iter()
                            .map(|block| crate::ast::TrailingBlockDecl {
                                exposure: block.exposure,
                                label: block.label,
                                meta: Meta::new(block.meta.span),
                            })
                            .collect(),
                        captures,
                        call_ty: self.walk_type(call_ty)?,
                        schedule,
                        implementation,
                        body_trivia,
                        meta: Meta::new(span),
                        doc,
                    },
                    (),
                )])
            }
            // Host items are inhabited at every phase — signature-only
            // pass-through, no labels to elaborate.
            Item::HostType(h) => Ok(vec![Item::HostType(self.walk_host_type(h)?)]),
            Item::HostFn(h) => Ok(vec![Item::HostFn(self.walk_host_fn(h)?)]),
            // Statically uninhabited at Desugared.
            Item::Op(_, ext) => match ext {},
            Item::RecGroup(_, ext) => match ext {},
        }
    }

    fn walk_fn_def(&mut self, d: FnDef<Desugared>) -> Result<FnDef<Lowered>, Error> {
        let FnDef {
            vis,
            purity,
            name,
            sig,
            ret,
            ret_elided,
            body,
            meta: Meta { span, .. },
            doc,
        } = d;
        Ok(FnDef {
            vis,
            purity,
            name,
            sig: Signature::from_parts(self.walk_signature_params(sig.params)?, sig.groups),
            ret: self.walk_type(ret)?,
            ret_elided,
            body: self.walk_expr(body)?,
            meta: Meta::new(span),
            doc,
        })
    }

    /// Walk an `TypeAlias` Desugared → Lowered.
    fn walk_alias(&mut self, a: TypeAlias<Desugared>) -> Result<TypeAlias<Lowered>, Error> {
        let TypeAlias {
            vis,
            name,
            name_span,
            type_params,
            body,
            meta: Meta { span, .. },
            editable_span,
            doc,
        } = a;
        Ok(TypeAlias {
            vis,
            name,
            name_span,
            type_params,
            body: self.walk_type(body)?,
            meta: Meta::new(span),
            editable_span,
            doc,
        })
    }

    fn walk_newtype(&mut self, d: Newtype<Desugared>) -> Result<Newtype<Lowered>, Error> {
        let Newtype {
            vis,
            rec_span,
            name,
            name_span,
            type_params,
            existential_params,
            payload,
            constructor,
            projector,
            meta: Meta { span, .. },
            editable_span,
            doc,
        } = d;
        Ok(Newtype {
            vis,
            rec_span,
            name,
            name_span,
            type_params,
            existential_params,
            payload: self.walk_type(payload)?,
            constructor: convert_type_member(&constructor),
            projector: convert_type_member(&projector),
            meta: Meta::new(span),
            editable_span,
            doc,
        })
    }

    fn walk_equiv(&mut self, e: Equiv<Desugared>) -> Result<Equiv<Lowered>, Error> {
        let Equiv {
            name,
            name_span,
            sig,
            terms,
            meta: Meta { span, .. },
        } = e;
        let terms: Vec<crate::ast::EquivTerm<Lowered>> = terms
            .into_iter()
            .map(|t| {
                let crate::ast::EquivTerm {
                    body,
                    meta: Meta { span: tspan, .. },
                } = t;
                Ok(crate::ast::EquivTerm {
                    body: self.walk_expr(body)?,
                    meta: Meta::new(tspan),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Equiv {
            name,
            name_span,
            sig: Signature::from_parts(self.walk_signature_params(sig.params)?, sig.groups),
            terms,
            meta: Meta::new(span),
        })
    }

    fn walk_host_type(&mut self, h: HostType<Desugared>) -> Result<HostType<Lowered>, Error> {
        let HostType {
            name,
            type_params,
            role,
            owned,
            meta: Meta { span, .. },
            doc,
        } = h;
        Ok(HostType {
            name,
            type_params,
            role,
            owned,
            meta: Meta::new(span),
            doc,
        })
    }

    fn walk_host_fn(&mut self, h: HostFn<Desugared>) -> Result<HostFn<Lowered>, Error> {
        let HostFn {
            name,
            params,
            param_groups,
            ret,
            meta: Meta { span, .. },
            doc,
        } = h;
        let params: Vec<HostFnParam<Lowered>> = params
            .into_iter()
            .map(|p| self.walk_host_fn_param(p))
            .collect::<Result<_, _>>()?;
        Ok(HostFn {
            name,
            params,
            param_groups,
            ret: self.walk_type(ret)?,
            meta: Meta::new(span),
            doc,
        })
    }

    fn walk_host_fn_param(
        &mut self,
        p: HostFnParam<Desugared>,
    ) -> Result<HostFnParam<Lowered>, Error> {
        Ok(match p {
            HostFnParam::Type(tp) => HostFnParam::Type(tp),
            HostFnParam::Value(v) => HostFnParam::Value(HostFnValueParam {
                name: v.name,
                ty: self.walk_type(v.ty)?,
                meta: Meta::new(v.meta.span),
            }),
        })
    }

    fn walk_signature_params(
        &mut self,
        params: Vec<SignatureParam<Desugared>>,
    ) -> Result<Vec<SignatureParam<Lowered>>, Error> {
        params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect()
    }

    fn walk_signature_param(
        &mut self,
        p: SignatureParam<Desugared>,
    ) -> Result<SignatureParam<Lowered>, Error> {
        Ok(match p {
            SignatureParam::Type(tp) => SignatureParam::Type(tp),
            SignatureParam::Value(v) => SignatureParam::Value(self.walk_param(v)?),
        })
    }

    fn walk_param(&mut self, p: Param<Desugared>) -> Result<Param<Lowered>, Error> {
        let Param {
            name,
            ty,
            pattern: _,
            meta: Meta { span, .. },
        } = p;
        Ok(Param {
            name,
            ty: ty.map(|t| self.walk_type(t)).transpose()?,
            pattern: (),
            meta: Meta::new(span),
        })
    }

    fn walk_call_arg(&mut self, a: CallArg<Desugared>) -> Result<CallArg<Lowered>, Error> {
        Ok(match a {
            CallArg::Type(t) => CallArg::Type(self.walk_type(t)?),
            CallArg::Value(e) => CallArg::Value(self.walk_expr(e)?),
        })
    }

    fn walk_expr(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        match e {
            Expr::Path { .. } => self.visit_expr_path(e),
            Expr::Call { .. } => self.visit_expr_call(e),
            Expr::FnExpr { .. } => self.visit_expr_fn(e),
            Expr::Let { .. } => self.visit_expr_let(e),
            Expr::RowLet { ext, .. } => match ext {},
            Expr::Seq { .. } => self.visit_expr_seq(e),
            Expr::Unit { .. } => self.visit_expr_unit(e),
            Expr::StrLit { .. } => self.visit_expr_str_lit(e),
            Expr::IntLit { .. } => self.visit_expr_int_lit(e),
            Expr::FloatLit { .. } => self.visit_expr_float_lit(e),
            Expr::BoolLit { .. } => self.visit_expr_bool_lit(e),
            Expr::Elaborator { .. } => self.visit_expr_elaborator(e),
            Expr::RecOrder { .. } => self.visit_expr_rec_order(e),
            Expr::RecQuote {
                plan, meta, ext, ..
            } => {
                use crate::ast::RecQuotePlan;
                let plan = match *plan {
                    RecQuotePlan::Operand {
                        public_ty,
                        runtime_ty,
                        computation,
                    } => RecQuotePlan::Operand {
                        public_ty: public_ty.map(|ty| self.walk_type(ty)).transpose()?,
                        runtime_ty: self.walk_type(runtime_ty)?,
                        computation: Box::new(self.walk_expr(*computation)?),
                    },
                    RecQuotePlan::Expansion {
                        runtime_ty,
                        continuation,
                        value,
                    } => RecQuotePlan::Expansion {
                        runtime_ty: self.walk_type(runtime_ty)?,
                        continuation: Box::new(self.walk_expr(*continuation)?),
                        value: Box::new(self.walk_expr(*value)?),
                    },
                };
                Ok(Expr::RecQuote {
                    occurrence: Default::default(),
                    plan: Box::new(plan),
                    meta: Meta::new(meta.span),
                    ext,
                })
            }
            Expr::UserElaborator { .. } => self.visit_expr_user_elaborator(e),
            Expr::BlockCall { ext, .. } => match ext {},
            Expr::LabelValue {
                occurrence: _,
                labels,
                meta,
                ext,
            } => self.rewrite_expr_label_value(labels, meta, ext),
            // Statically uninhabited at Desugared.
            Expr::Tuple { ext, .. } => match ext {},
            Expr::FnPlaceholder { ext, .. } => match ext {},
            Expr::Ufcs { .. } => self.visit_expr_ufcs(e),
            Expr::RecCall { ext, .. } => match ext {},
            Expr::OpChain { ext, .. } => match ext {},
        }
    }

    fn walk_type(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        match ty {
            Type::Path { .. } => self.visit_type_path(ty),
            Type::Unit { .. } => self.visit_type_unit(ty),
            Type::Bottom { .. } => self.visit_type_bottom(ty),
            Type::Function { .. } => self.visit_type_function(ty),
            Type::Product { .. } => self.visit_type_product(ty),
            Type::Sum { .. } => self.visit_type_sum(ty),
            Type::Forall { .. } => self.visit_type_forall(ty),
            Type::Infer { .. } => self.visit_type_infer(ty),
            Type::LabelSugar { labels, meta, .. } => self.rewrite_type_label_sugar(labels, meta),
        }
    }
}

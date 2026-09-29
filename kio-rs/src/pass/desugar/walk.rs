//! Surface → Desugared walker trait.
//!
//! The [`SurfaceToDesugared`] trait pairs every Surface AST variant
//! with a `visit_*` method that converts it to its Desugared
//! counterpart. Variants inhabited at Desugared have default
//! implementations that clone-and-recurse via `self.walk_*`.
//! Variants uninhabited at Desugared — `Expr::Tuple`,
//! `Expr::FnPlaceholder`, `Expr::BlockCall`, `Item::Op`,
//! `Item::Fold`, and `Item::LiteralAlias` — have no default
//! `rewrite_*` methods that the implementer must provide.
//!
//! `walk_item` returns `Vec<Item<Desugared>>` so an implementer
//! may drop items (e.g., [`crate::pass::desugar`] drops `Item::Op`,
//! `Item::Fold`, and `Item::LiteralAlias` declarations after
//! consuming them into the lowering state).
//!
//! Inputs are taken owned (matching desugar's existing API). The
//! parser produces a Surface AST that is consumed by desugar to
//! produce the Desugared AST; no other consumer needs the Surface
//! AST after desugar runs.

use crate::ast::{
    CallArg, Desugared, ElaboratorCall, Equiv, Expr, FieldAccessLabel, FieldUpdateLabel, FnDef,
    HostFn, HostFnParam, HostFnValueParam, HostType, Item, LabelEntry, LabelSugarLabel,
    LabelValueLabel, Labels, LabelsArm, LiteralAlias, Meta, Module, Op, PackageFile, Param,
    PathSegment, PlaceholderState, RecGroup, RowLetEntry, Signature, SignatureParam, Surface, Type,
    TypeAlias, VariadicOperator, convert_type_member,
};
use crate::error::Error;

/// Walker over `Surface → Desugared`. Methods named `visit_*` have
/// default clone-and-recurse implementations. Methods named
/// `rewrite_*` have no default — they handle variants that are
/// uninhabited at Desugared, and the implementer must provide a
/// rule for each.
pub trait SurfaceToDesugared {
    // -------------------------------------------------------------
    // Rewrite hooks (no defaults).
    // -------------------------------------------------------------

    fn rewrite_expr_tuple(
        &mut self,
        items: Vec<Expr<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error>;

    fn rewrite_expr_fn_placeholder(
        &mut self,
        body: Box<Expr<Surface>>,
        stem: PathSegment,
        state: PlaceholderState,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error>;

    fn rewrite_expr_op_chain(
        &mut self,
        kind: crate::ast::OpChainKind<Surface>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error>;

    fn rewrite_expr_row_let(
        &mut self,
        entries: Vec<RowLetEntry<Surface>>,
        value: Box<Expr<Surface>>,
        body: Box<Expr<Surface>>,
        temp_name: String,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error>;

    /// `Expr::RecCall` is uninhabited at Desugared — the
    /// recursion-group rewrite consumes tail-position marked calls
    /// and any call that reaches the ordinary walker is a surface
    /// error.
    fn rewrite_expr_rec_call(
        &mut self,
        modes: Vec<crate::ast::RecCallMode>,
        callee: PathSegment,
        args: Vec<CallArg<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Desugared>, Error>;

    /// `Item::Op` may be dropped (return `Ok(vec![])`),
    /// transformed (return `Ok(vec![item])`), or expanded; desugar
    /// drops these after consuming the operator table.
    fn rewrite_item_op(&mut self, d: Op<Surface>) -> Result<Vec<Item<Desugared>>, Error>;

    /// `Item::Fold` is surface-only and may be dropped after the
    /// operator-fold pass has consumed it.
    fn rewrite_item_fold(
        &mut self,
        d: VariadicOperator<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error>;

    /// A `rec(loop)` group is surface-only. Desugar validates its
    /// marked calls and lowers the group to ordinary `fn` / `newtype`
    /// items plus a call to the named loop function.
    fn rewrite_item_rec_group(
        &mut self,
        d: RecGroup<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error>;

    /// A `literal` item may be dropped or transformed; desugar drops
    /// these after consuming the literal substitution table.
    fn rewrite_item_literal_alias(
        &mut self,
        a: LiteralAlias<Surface>,
    ) -> Result<Vec<Item<Desugared>>, Error>;

    // -------------------------------------------------------------
    // Per-variant visit methods (default-recurse).
    //
    // The walk_* dispatchers route each AST variant to its own visit_*
    // method; the let-else at a method's head can fail only if a
    // dispatcher routed the wrong variant — a bug in this file, hence
    // the bare `unreachable!()`.
    // -------------------------------------------------------------

    fn visit_expr_path(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::Path {
            occurrence: _,
            segments,
            meta,
            ext: _,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Path {
            occurrence: Default::default(),
            segments,
            meta: Meta::new(meta.span),
            ext: (),
        })
    }

    fn visit_expr_call(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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
        let args: Vec<CallArg<Desugared>> = args
            .into_iter()
            .map(|a| self.walk_call_arg(a))
            .collect::<Result<_, _>>()?;
        Ok(Expr::synth_call(callee, args, span))
    }

    fn visit_expr_fn(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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
        let params: Vec<SignatureParam<Desugared>> = sig
            .params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect::<Result<_, _>>()?;
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, sig.groups),
            ret_ty: ret_ty.map(|t| self.walk_type(t)).transpose()?,
            body: Box::new(self.walk_expr(*body)?),
            meta: Meta::new(span),
            caps: (),
        })
    }

    fn visit_expr_let(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        if pattern.is_some() {
            return Err(Error::parse(
                span,
                "internal error: surface let pattern reached the generic desugar walker",
            ));
        }
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

    fn visit_expr_seq(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_unit(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_str_lit(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_int_lit(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_float_lit(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_bool_lit(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_label_value(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        let Expr::LabelValue {
            occurrence: _,
            labels,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        let labels: Vec<LabelValueLabel<Desugared>> = labels
            .into_iter()
            .map(|l| self.walk_label_value_label(l))
            .collect::<Result<_, _>>()?;
        Ok(Expr::LabelValue {
            occurrence: Default::default(),
            labels,
            meta: Meta::new(span),
            ext,
        })
    }

    fn visit_expr_elaborator(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_user_elaborator(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_expr_ufcs(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
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

    fn visit_type_path(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
        let Type::Path {
            segments,
            args,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        let args: Vec<Type<Desugared>> = args
            .into_iter()
            .map(|a| self.walk_type(a))
            .collect::<Result<_, _>>()?;
        Ok(Type::synth_path_segments(segments, args, span))
    }

    fn visit_type_unit(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_type_bottom(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_type_function(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_type_product(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_type_sum(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_type_forall(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_type_label_sugar(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
        let Type::LabelSugar {
            labels,
            meta: Meta { span, .. },
            ext,
        } = ty
        else {
            unreachable!()
        };
        let labels: Vec<LabelSugarLabel<Desugared>> = labels
            .into_iter()
            .map(|l| self.walk_label_sugar_label(l))
            .collect::<Result<_, _>>()?;
        Ok(Type::LabelSugar {
            labels,
            meta: Meta::new(span),
            ext,
        })
    }

    fn visit_type_infer(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
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

    fn visit_item_fn_def(&mut self, d: FnDef<Surface>) -> Result<Item<Desugared>, Error> {
        Ok(Item::FnDef(self.walk_fn_def(d)?))
    }

    /// Convert a `type` item.
    fn visit_item_type_alias(&mut self, a: TypeAlias<Surface>) -> Result<Item<Desugared>, Error> {
        Ok(Item::TypeAlias(self.walk_type_alias(a)?))
    }

    fn visit_item_newtype(
        &mut self,
        d: crate::ast::Newtype<Surface>,
    ) -> Result<Item<Desugared>, Error> {
        Ok(Item::Newtype(self.walk_newtype(d)?))
    }

    fn visit_item_labels(&mut self, d: Labels<Surface>) -> Result<Item<Desugared>, Error> {
        Ok(Item::Labels(self.walk_labels(d)?, ()))
    }

    fn visit_item_label_forward(
        &mut self,
        forward: crate::ast::LabelForward<Surface>,
    ) -> Result<Item<Desugared>, Error> {
        Ok(Item::LabelForward(
            crate::ast::LabelForward {
                vis: forward.vis,
                name: forward.name,
                name_span: forward.name_span,
                target: forward.target,
                target_span: forward.target_span,
                body_trivia: Default::default(),
                meta: Meta::new(forward.meta.span),
                editable_span: forward.editable_span,
                doc: forward.doc,
            },
            (),
        ))
    }

    fn visit_item_equiv(&mut self, e: Equiv<Surface>) -> Result<Item<Desugared>, Error> {
        Ok(Item::Equiv(self.walk_equiv(e)?, ()))
    }

    fn visit_item_elaborator(
        &mut self,
        s: crate::ast::UserElaboratorDef<Surface>,
    ) -> Result<Item<Desugared>, Error> {
        let crate::ast::UserElaboratorDef {
            vis,
            name,
            name_span,
            trailing_blocks,
            captures,
            call_ty,
            schedule,
            implementation,
            body_trivia: _,
            meta: Meta { span, .. },
            doc,
        } = s;
        Ok(Item::Elaborator(
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
                body_trivia: Default::default(),
                meta: Meta::new(span),
                doc,
            },
            (),
        ))
    }

    // -------------------------------------------------------------
    // Dispatchers and sub-walks (default impls).
    // -------------------------------------------------------------

    fn walk_module(&mut self, module: Module<Surface>) -> Result<Module<Desugared>, Error> {
        let Module {
            path,
            imports,
            items,
            meta: Meta { span, .. },
            doc,
        } = module;
        let mut new_items: Vec<Item<Desugared>> = Vec::with_capacity(items.len());
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
        package: PackageFile<Surface>,
    ) -> Result<PackageFile<Desugared>, Error> {
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

    fn walk_item(&mut self, item: Item<Surface>) -> Result<Vec<Item<Desugared>>, Error> {
        match item {
            Item::FnDef(d) => Ok(vec![self.visit_item_fn_def(d)?]),
            Item::TypeAlias(a) => Ok(vec![self.visit_item_type_alias(a)?]),
            Item::LiteralAlias(a, _) => self.rewrite_item_literal_alias(a),
            Item::Newtype(d) => Ok(vec![self.visit_item_newtype(d)?]),
            Item::Labels(d, _) => Ok(vec![self.visit_item_labels(d)?]),
            Item::LabelForward(d, _) => Ok(vec![self.visit_item_label_forward(d)?]),
            Item::Equiv(e, _) => Ok(vec![self.visit_item_equiv(e)?]),
            Item::Elaborator(s, _) => Ok(vec![self.visit_item_elaborator(s)?]),
            Item::RecGroup(d, _) => self.rewrite_item_rec_group(d),
            Item::TypeRecGroup(group) => {
                let mut members = Vec::with_capacity(group.members.len());
                for member in group.members {
                    members.push(match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            crate::ast::TypeRecMember::TypeAlias(self.walk_type_alias(alias)?)
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            crate::ast::TypeRecMember::Newtype(self.walk_newtype(newtype)?)
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            crate::ast::TypeRecMember::Labels(self.walk_labels(labels)?, ())
                        }
                    });
                }
                Ok(vec![Item::TypeRecGroup(crate::ast::TypeRecGroup {
                    members,
                    doc: group.doc.clone(),
                    source_layout: group.source_layout.clone(),
                    rec_span: group.rec_span,
                    open_brace_span: group.open_brace_span,
                    close_brace_span: group.close_brace_span,
                    deferred_rec_labels_diagnostic: group.deferred_rec_labels_diagnostic.clone(),
                    meta: crate::ast::convert_meta(&group.meta),
                })])
            }
            Item::Op(d, _) => self.rewrite_item_op(*d),
            Item::VariadicOperator(d, _) => self.rewrite_item_fold(*d),
            // Host items are inhabited at every phase — signature-only
            // pass-through, no exprs to desugar.
            Item::HostType(h) => Ok(vec![Item::HostType(self.walk_host_type(h)?)]),
            Item::HostFn(h) => Ok(vec![Item::HostFn(self.walk_host_fn(h)?)]),
        }
    }

    fn walk_fn_def(&mut self, d: FnDef<Surface>) -> Result<FnDef<Desugared>, Error> {
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

    /// Walk a `type` Surface → Desugared.
    fn walk_type_alias(&mut self, a: TypeAlias<Surface>) -> Result<TypeAlias<Desugared>, Error> {
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

    fn walk_newtype(
        &mut self,
        d: crate::ast::Newtype<Surface>,
    ) -> Result<crate::ast::Newtype<Desugared>, Error> {
        let crate::ast::Newtype {
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
        Ok(crate::ast::Newtype {
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

    fn walk_labels(&mut self, d: Labels<Surface>) -> Result<Labels<Desugared>, Error> {
        let Labels {
            vis,
            rec_span,
            type_alias_name,
            type_alias_span,
            type_alias_params,
            type_alias_arms,
            entries,
            meta: Meta { span, .. },
            editable_span,
            doc,
        } = d;
        let type_alias_arms = type_alias_arms
            .map(|arms| {
                arms.into_iter()
                    .map(|arm| self.walk_labels_arm(arm))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let entries: Vec<LabelEntry<Desugared>> = entries
            .into_iter()
            .map(|e| self.walk_label_entry(e))
            .collect::<Result<_, _>>()?;
        Ok(Labels {
            vis,
            rec_span,
            type_alias_name,
            type_alias_span,
            type_alias_params,
            type_alias_arms,
            entries,
            meta: Meta::new(span),
            editable_span,
            doc,
        })
    }

    fn walk_labels_arm(&mut self, arm: LabelsArm<Surface>) -> Result<LabelsArm<Desugared>, Error> {
        let LabelsArm {
            entries,
            meta: Meta { span, .. },
        } = arm;
        let entries = entries
            .into_iter()
            .map(|e| self.walk_label_entry(e))
            .collect::<Result<_, _>>()?;
        Ok(LabelsArm {
            entries,
            meta: Meta::new(span),
        })
    }

    fn walk_label_entry(&mut self, e: LabelEntry<Surface>) -> Result<LabelEntry<Desugared>, Error> {
        let LabelEntry {
            name,
            name_span,
            type_params,
            existential_params,
            payload,
            meta: Meta { span, .. },
        } = e;
        Ok(LabelEntry {
            name,
            name_span,
            type_params,
            existential_params,
            payload: self.walk_type(payload)?,
            meta: Meta::new(span),
        })
    }

    fn walk_equiv(&mut self, e: Equiv<Surface>) -> Result<Equiv<Desugared>, Error> {
        let Equiv {
            name,
            name_span,
            sig,
            terms,
            meta: Meta { span, .. },
        } = e;
        let terms: Vec<crate::ast::EquivTerm<Desugared>> = terms
            .into_iter()
            .map(|t| {
                let crate::ast::EquivTerm {
                    body,
                    meta: Meta { span, .. },
                } = t;
                Ok(crate::ast::EquivTerm {
                    body: self.walk_expr(body)?,
                    meta: Meta::new(span),
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

    fn walk_host_type(&mut self, h: HostType<Surface>) -> Result<HostType<Desugared>, Error> {
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

    fn walk_host_fn(&mut self, h: HostFn<Surface>) -> Result<HostFn<Desugared>, Error> {
        let HostFn {
            name,
            params,
            param_groups,
            ret,
            meta: Meta { span, .. },
            doc,
        } = h;
        let params: Vec<HostFnParam<Desugared>> = params
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
        p: HostFnParam<Surface>,
    ) -> Result<HostFnParam<Desugared>, Error> {
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
        params: Vec<SignatureParam<Surface>>,
    ) -> Result<Vec<SignatureParam<Desugared>>, Error> {
        params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect()
    }

    fn walk_signature_param(
        &mut self,
        p: SignatureParam<Surface>,
    ) -> Result<SignatureParam<Desugared>, Error> {
        Ok(match p {
            SignatureParam::Type(tp) => SignatureParam::Type(tp),
            SignatureParam::Value(v) => SignatureParam::Value(self.walk_param(v)?),
        })
    }

    fn walk_param(&mut self, p: Param<Surface>) -> Result<Param<Desugared>, Error> {
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

    fn walk_call_arg(&mut self, a: CallArg<Surface>) -> Result<CallArg<Desugared>, Error> {
        Ok(match a {
            CallArg::Type(t) => CallArg::Type(self.walk_type(t)?),
            CallArg::Value(e) => CallArg::Value(self.walk_expr(e)?),
        })
    }

    fn walk_label_value_label(
        &mut self,
        l: LabelValueLabel<Surface>,
    ) -> Result<LabelValueLabel<Desugared>, Error> {
        let LabelValueLabel {
            label,
            label_span,
            value,
            meta: Meta { span, .. },
        } = l;
        Ok(LabelValueLabel {
            label,
            label_span,
            value: self.walk_expr(value)?,
            meta: Meta::new(span),
        })
    }

    fn walk_label_sugar_label(
        &mut self,
        l: LabelSugarLabel<Surface>,
    ) -> Result<LabelSugarLabel<Desugared>, Error> {
        let LabelSugarLabel {
            label,
            label_span,
            payload,
            meta: Meta { span, .. },
        } = l;
        Ok(LabelSugarLabel {
            label,
            label_span,
            payload: payload.map(|p| self.walk_type(p)).transpose()?,
            meta: Meta::new(span),
        })
    }

    fn walk_expr(&mut self, e: Expr<Surface>) -> Result<Expr<Desugared>, Error> {
        match e {
            Expr::Path { .. } => self.visit_expr_path(e),
            Expr::Call { .. } => self.visit_expr_call(e),
            Expr::RecCall {
                modes,
                callee,
                args,
                meta,
                ..
            } => self.rewrite_expr_rec_call(modes, callee, args, meta),
            Expr::FnExpr { .. } => self.visit_expr_fn(e),
            Expr::Let { .. } => self.visit_expr_let(e),
            Expr::RowLet {
                entries,
                value,
                body,
                temp_name,
                meta,
                ..
            } => self.rewrite_expr_row_let(entries, value, body, temp_name, meta),
            Expr::Seq { .. } => self.visit_expr_seq(e),
            Expr::Unit { .. } => self.visit_expr_unit(e),
            Expr::StrLit { .. } => self.visit_expr_str_lit(e),
            Expr::IntLit { .. } => self.visit_expr_int_lit(e),
            Expr::FloatLit { .. } => self.visit_expr_float_lit(e),
            Expr::BoolLit { .. } => self.visit_expr_bool_lit(e),
            Expr::LabelValue { .. } => self.visit_expr_label_value(e),
            Expr::Elaborator { .. } => self.visit_expr_elaborator(e),
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match ext {},
            Expr::UserElaborator { .. } => self.visit_expr_user_elaborator(e),
            Expr::BlockCall { .. } => {
                unreachable!("trailing blocks are projected before desugaring")
            }
            // Required rewrites.
            Expr::Tuple { items, meta, .. } => self.rewrite_expr_tuple(items, meta),
            Expr::FnPlaceholder {
                body,
                stem,
                state,
                meta,
                ..
            } => self.rewrite_expr_fn_placeholder(body, stem, state, meta),
            Expr::OpChain { kind, meta, .. } => self.rewrite_expr_op_chain(kind, meta),
            Expr::Ufcs { .. } => self.visit_expr_ufcs(e),
        }
    }

    fn walk_type(&mut self, ty: Type<Surface>) -> Result<Type<Desugared>, Error> {
        match ty {
            Type::Path { .. } => self.visit_type_path(ty),
            Type::Unit { .. } => self.visit_type_unit(ty),
            Type::Bottom { .. } => self.visit_type_bottom(ty),
            Type::Function { .. } => self.visit_type_function(ty),
            Type::Product { .. } => self.visit_type_product(ty),
            Type::Sum { .. } => self.visit_type_sum(ty),
            Type::Forall { .. } => self.visit_type_forall(ty),
            Type::LabelSugar { .. } => self.visit_type_label_sugar(ty),
            Type::Infer { .. } => self.visit_type_infer(ty),
        }
    }
}

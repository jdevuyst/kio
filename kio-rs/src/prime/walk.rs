//! Surface → Prime walker trait.
//!
//! The [`SurfaceToPrime`] trait covers every Surface AST variant.
//! Methods
//! for variants that are inhabited at Prime (`Path`, `Call`, `FnExpr`,
//! literals, …) have default implementations that clone-and-recurse
//! via `self.walk_*`. Methods for variants that are uninhabited at
//! Prime (tuple, conditional, elaborator, UFCS, placeholder, label, recursive,
//! operator, and `do` extensions on expressions; `LabelSugar` / `Infer` on
//! types; `Labels`, `Equiv`, `Op`, `Fold`, term-level `RecGroup`, elaborator
//! declarations, and `LiteralAlias` on items; surface-only import entries)
//! have **no default** — the implementer must provide one (typically
//! rejecting the variant with `Error::Parse`). `RowLet` is rejected directly by
//! the dispatcher, while `RecOrder` is discharged there as statically
//! uninhabited; neither has a visitor hook. Type-recursive declaration groups
//! are Kio' and use the ordinary surviving-item path.
//!
//! The trait's `walk_module` / `walk_package_file` / `walk_item` /
//! `walk_expr` / `walk_type` dispatchers do the per-variant `match`
//! and route surviving and hook-backed variants to their `lower_*` or
//! `rewrite_*` methods, with the two direct cases handled in place.
//!
//! [`crate::prime::lower`] implements this trait; the kio-prime binary's
//! Surface → Prime entry points
//! [`crate::prime::lower::lower_module`] /
//! [`crate::prime::lower::lower_package_file`] are thin wrappers
//! around `walk_module` / `walk_package_file` on an implementer
//! instance.

use crate::ast::{
    CallArg, Equiv, Expr, FnDef, HostFn, HostFnParam, HostFnValueParam, HostType, Item,
    LabelSugarLabel, LabelValueLabel, Labels, LiteralAlias, Meta, Module, Newtype, Op, PackageFile,
    Param, PathSegment, PlaceholderState, Prime, RecGroup, Signature, SignatureParam, Surface,
    Type, TypeAlias, VariadicOperator, convert_type_member,
};
use crate::error::Error;
use crate::span::Span;

pub struct SurfaceUfcs {
    pub receiver: Box<Expr<Surface>>,
    pub callee_segments: Vec<PathSegment>,
    pub callee_span: Span,
    pub args: Vec<CallArg<Surface>>,
    pub flavor: crate::ast::UfcsFlavor,
    pub bang: Option<Span>,
    pub meta: Meta<Surface>,
}

/// Walker over `Surface → Prime`. Methods named `lower_*` have
/// default implementations that clone-and-recurse (the variant
/// survives into Prime). Methods named `rewrite_*` have no default
/// — they handle variants that are uninhabited at Prime, and the
/// implementer must provide a rule for each (typically rejecting
/// the variant).
pub trait SurfaceToPrime {
    // -------------------------------------------------------------
    // Rewrite hooks (no defaults). Each surface-only variant goes
    // through one of these; the implementer must spell out what to
    // do with it.
    // -------------------------------------------------------------

    fn rewrite_expr_tuple(
        &mut self,
        items: Vec<Expr<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_fn_placeholder(
        &mut self,
        body: Box<Expr<Surface>>,
        stem: PathSegment,
        state: PlaceholderState,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_op_chain(
        &mut self,
        kind: crate::ast::OpChainKind<Surface>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_rec_call(
        &mut self,
        callee: PathSegment,
        args: Vec<CallArg<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_label_value(
        &mut self,
        labels: Vec<LabelValueLabel<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_elaborator(
        &mut self,
        kind: crate::ast::ElaboratorKind,
        call: crate::ast::ElaboratorCall<Surface>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_user_elaborator(
        &mut self,
        name: String,
        args: Vec<crate::ast::CallArg<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error>;

    fn rewrite_expr_ufcs(&mut self, ufcs: SurfaceUfcs) -> Result<Expr<Prime>, Error>;

    fn rewrite_type_label_sugar(
        &mut self,
        labels: Vec<LabelSugarLabel<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Type<Prime>, Error>;

    fn rewrite_type_infer(&mut self, meta: Meta<Surface>) -> Result<Type<Prime>, Error>;

    fn rewrite_item_labels(&mut self, d: Labels<Surface>) -> Result<Item<Prime>, Error>;

    fn rewrite_item_label_forward(
        &mut self,
        forward: crate::ast::LabelForward<Surface>,
    ) -> Result<Item<Prime>, Error>;

    fn rewrite_type_rec_member_labels(
        &mut self,
        d: Labels<Surface>,
    ) -> Result<crate::ast::TypeRecMember<Prime>, Error>;

    fn rewrite_item_equiv(&mut self, e: Equiv<Surface>) -> Result<Item<Prime>, Error>;

    fn rewrite_item_op(&mut self, d: Op<Surface>) -> Result<Item<Prime>, Error>;

    fn rewrite_item_fold(&mut self, d: VariadicOperator<Surface>) -> Result<Item<Prime>, Error>;

    fn rewrite_item_rec_group(&mut self, d: RecGroup<Surface>) -> Result<Item<Prime>, Error>;

    fn rewrite_item_elaborator(
        &mut self,
        d: crate::ast::UserElaboratorDef<Surface>,
    ) -> Result<Item<Prime>, Error>;

    /// Reject a `literal` item.
    fn rewrite_item_literal_alias(
        &mut self,
        a: LiteralAlias<Surface>,
    ) -> Result<Item<Prime>, Error>;

    /// Reject an operator-pattern item in an `import` clause. Surface-
    /// only because operator dispatch isn't in Kio' (`op` is
    /// rejected by [`Self::rewrite_item_op`], so importing one
    /// must be rejected here for symmetry).
    fn rewrite_import_op_pattern(&mut self, span: Span) -> Result<(), Error>;

    /// Reject a braced label import. Label imports are surface-only and the
    /// label elaborator must consume them before Kio'.
    fn rewrite_import_label(&mut self, span: Span) -> Result<(), Error>;

    // -------------------------------------------------------------
    // Dispatchers (default impls). Implementers rarely override
    // these; they dispatch each variant to either a `lower_*`
    // clone-and-recurse method (inhabited at Prime) or a `rewrite_*`
    // hook (uninhabited at Prime).
    // -------------------------------------------------------------

    fn walk_module(&mut self, module: Module<Surface>) -> Result<Module<Prime>, Error> {
        let Module {
            path,
            imports,
            items,
            meta: Meta { span, .. },
            doc,
        } = module;
        self.check_imports(&imports)?;
        let items: Vec<Item<Prime>> = items
            .into_iter()
            .map(|i| self.walk_item(i))
            .collect::<Result<_, _>>()?;
        Ok(Module {
            path,
            imports,
            items,
            meta: Meta::new(span),
            doc,
        })
    }

    fn walk_package_file(
        &mut self,
        package: PackageFile<Surface>,
    ) -> Result<PackageFile<Prime>, Error> {
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

    /// Reject every surface-only selective import item at the Kio' boundary.
    fn check_imports(&mut self, imports: &[crate::ast::Import]) -> Result<(), Error> {
        for u in imports {
            if let crate::ast::ImportKind::Selective { items, .. } = &u.kind {
                for it in items {
                    match it {
                        crate::ast::ImportItem::Label { span, .. } => {
                            self.rewrite_import_label(*span)?;
                        }
                        crate::ast::ImportItem::OperatorPattern { span, .. } => {
                            self.rewrite_import_op_pattern(*span)?;
                        }
                        crate::ast::ImportItem::Name { .. } => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn walk_item(&mut self, item: Item<Surface>) -> Result<Item<Prime>, Error> {
        match item {
            Item::FnDef(d) => Ok(Item::FnDef(self.walk_fn_def(d)?)),
            Item::TypeAlias(a) => Ok(Item::TypeAlias(self.walk_type_alias(a)?)),
            Item::LiteralAlias(a, _) => self.rewrite_item_literal_alias(a),
            Item::Newtype(d) => Ok(Item::Newtype(self.walk_newtype(d)?)),
            Item::Labels(d, _) => self.rewrite_item_labels(d),
            Item::LabelForward(d, _) => self.rewrite_item_label_forward(d),
            Item::Equiv(e, _) => self.rewrite_item_equiv(e),
            Item::Op(d, _) => self.rewrite_item_op(*d),
            Item::VariadicOperator(d, _) => self.rewrite_item_fold(*d),
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
                            self.rewrite_type_rec_member_labels(labels)?
                        }
                    });
                }
                Ok(Item::TypeRecGroup(crate::ast::TypeRecGroup {
                    members,
                    doc: group.doc.clone(),
                    source_layout: group.source_layout.clone(),
                    rec_span: group.rec_span,
                    open_brace_span: group.open_brace_span,
                    close_brace_span: group.close_brace_span,
                    deferred_rec_labels_diagnostic: group.deferred_rec_labels_diagnostic.clone(),
                    meta: crate::ast::convert_meta(&group.meta),
                }))
            }
            Item::Elaborator(d, _) => self.rewrite_item_elaborator(d),
            // Host items are NOT surface-only: they are inhabited in
            // Kio' as opaque declarations (`h : T`), so `prime::lower`
            // passes them straight through with no rejection.
            Item::HostType(h) => Ok(Item::HostType(self.walk_host_type(h)?)),
            Item::HostFn(h) => Ok(Item::HostFn(self.walk_host_fn(h)?)),
        }
    }

    fn walk_fn_def(&mut self, d: FnDef<Surface>) -> Result<FnDef<Prime>, Error> {
        let FnDef {
            vis,
            purity,
            name,
            sig,
            ret,
            ret_elided: _,
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
            // Surface-only provenance dropped at Prime.
            ret_elided: (),
            body: self.walk_expr(body)?,
            meta: Meta::new(span),
            doc,
        })
    }

    /// Walk a `type` Surface → Prime.
    fn walk_type_alias(&mut self, a: TypeAlias<Surface>) -> Result<TypeAlias<Prime>, Error> {
        let TypeAlias {
            vis,
            name,
            name_span,
            type_params,
            body,
            meta: Meta { span, .. },
            editable_span: _,
            doc,
        } = a;
        Ok(TypeAlias {
            vis,
            name,
            name_span,
            type_params,
            body: self.walk_type(body)?,
            meta: Meta::new(span),
            editable_span: None,
            doc,
        })
    }

    fn walk_newtype(&mut self, d: Newtype<Surface>) -> Result<Newtype<Prime>, Error> {
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
            editable_span: _,
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
            editable_span: None,
            doc,
        })
    }

    fn walk_host_type(&mut self, h: HostType<Surface>) -> Result<HostType<Prime>, Error> {
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

    fn walk_host_fn(&mut self, h: HostFn<Surface>) -> Result<HostFn<Prime>, Error> {
        let HostFn {
            name,
            params,
            param_groups,
            ret,
            meta: Meta { span, .. },
            doc,
        } = h;
        let params: Vec<HostFnParam<Prime>> = params
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

    fn walk_host_fn_param(&mut self, p: HostFnParam<Surface>) -> Result<HostFnParam<Prime>, Error> {
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
    ) -> Result<Vec<SignatureParam<Prime>>, Error> {
        params
            .into_iter()
            .map(|p| self.walk_signature_param(p))
            .collect()
    }

    fn walk_signature_param(
        &mut self,
        p: SignatureParam<Surface>,
    ) -> Result<SignatureParam<Prime>, Error> {
        Ok(match p {
            SignatureParam::Type(t) => SignatureParam::Type(t),
            SignatureParam::Value(v) => SignatureParam::Value(self.walk_param(v)?),
        })
    }

    fn walk_param(&mut self, p: Param<Surface>) -> Result<Param<Prime>, Error> {
        let Param {
            name,
            ty,
            pattern,
            meta: Meta { span, .. },
        } = p;
        if let Some(pat) = pattern {
            return Err(crate::error::Error::parse(
                pat.span,
                "parameter destructuring pattern is not part of Kio'. The `kio-prime` binary \
                     only accepts Kio'; use the `kio` binary to compile this program as full Kio."
                    .to_owned(),
            ));
        }
        Ok(Param {
            name,
            ty: ty.map(|t| self.walk_type(t)).transpose()?,
            pattern: (),
            meta: Meta::new(span),
        })
    }

    fn walk_call_arg(&mut self, a: CallArg<Surface>) -> Result<CallArg<Prime>, Error> {
        Ok(match a {
            CallArg::Type(t) => CallArg::Type(self.walk_type(t)?),
            CallArg::Value(v) => CallArg::Value(self.walk_expr(v)?),
        })
    }

    /// Walk a Kio' literal's mandatory `(Type)` annotation. Kio'
    /// requires an explicit annotation on every literal
    /// (`specs/grammar.md` § Kio' grammar), so the annotation is
    /// non-optional at `Prime`; a bare or `_`-annotated literal is a
    /// surface form the standalone Kio' pipeline rejects with a parse
    /// error.
    fn walk_literal_annotation(
        &mut self,
        annotation: Option<Type<Surface>>,
        span: Span,
    ) -> Result<Type<Prime>, Error> {
        match annotation {
            Some(t) if !matches!(t, Type::Infer { .. }) => self.walk_type(t),
            _ => Err(Error::parse(
                span,
                "Kio' requires a `(Type)` annotation on every literal \
                 (e.g. `42(I32)`, `\"hi\"(String)`); a bare literal is a surface form and \
                 is not valid Kio'",
            )),
        }
    }

    fn walk_expr(&mut self, e: Expr<Surface>) -> Result<Expr<Prime>, Error> {
        match e {
            Expr::Path {
                segments,
                meta: Meta { span, .. },
                ..
            } => Ok(Expr::Path {
                occurrence: Default::default(),
                segments,
                meta: Meta::new(span),
                ext: (),
            }),
            Expr::Call {
                callee,
                args,
                meta: Meta { span, .. },
                ..
            } => {
                let callee = self.walk_expr(*callee)?;
                let args: Vec<CallArg<Prime>> = args
                    .into_iter()
                    .map(|a| self.walk_call_arg(a))
                    .collect::<Result<_, _>>()?;
                Ok(Expr::synth_call(callee, args, span))
            }
            Expr::FnExpr {
                sig,
                ret_ty,
                body,
                meta: Meta { span, .. },
                ..
            } => Ok(Expr::FnExpr {
                occurrence: Default::default(),
                sig: Signature::from_parts(self.walk_signature_params(sig.params)?, sig.groups),
                ret_ty: ret_ty.map(|t| self.walk_type(t)).transpose()?,
                body: Box::new(self.walk_expr(*body)?),
                meta: Meta::new(span),
                caps: (),
            }),
            Expr::Let {
                occurrence: _,
                name,
                name_span,
                ty,
                pattern,
                value,
                body,
                meta: Meta { span, .. },
            } => {
                if pattern.is_some() {
                    return Err(Error::parse(
                        span,
                        "let destructuring is a surface form and is not valid Kio'",
                    ));
                }
                if ty.is_some() {
                    return Err(Error::parse(
                        span,
                        "let binder annotations are a surface form and are not valid Kio'",
                    ));
                }
                Ok(Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span,
                    ty: None,
                    pattern: (),
                    value: Box::new(self.walk_expr(*value)?),
                    body: Box::new(self.walk_expr(*body)?),
                    meta: Meta::new(span),
                })
            }
            Expr::Seq {
                occurrence: _,
                value,
                body,
                meta: Meta { span, .. },
            } => Ok(Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(self.walk_expr(*value)?),
                body: Box::new(self.walk_expr(*body)?),
                meta: Meta::new(span),
            }),
            Expr::Unit {
                occurrence: _,
                meta: Meta { span, .. },
            } => Ok(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            Expr::StrLit {
                occurrence: _,
                value,
                annotation,
                meta: Meta { span, .. },
            } => Ok(Expr::StrLit {
                occurrence: Default::default(),
                value,
                annotation: self.walk_literal_annotation(annotation, span)?,
                meta: Meta::new(span),
            }),
            Expr::IntLit {
                occurrence: _,
                digits,
                annotation,
                meta: Meta { span, .. },
            } => Ok(Expr::IntLit {
                occurrence: Default::default(),
                digits,
                annotation: self.walk_literal_annotation(annotation, span)?,
                meta: Meta::new(span),
            }),
            Expr::FloatLit {
                occurrence: _,
                digits,
                annotation,
                meta: Meta { span, .. },
            } => Ok(Expr::FloatLit {
                occurrence: Default::default(),
                digits,
                annotation: self.walk_literal_annotation(annotation, span)?,
                meta: Meta::new(span),
            }),
            Expr::BoolLit {
                occurrence: _,
                value,
                annotation,
                meta: Meta { span, .. },
            } => Ok(Expr::BoolLit {
                occurrence: Default::default(),
                value,
                annotation: self.walk_literal_annotation(annotation, span)?,
                meta: Meta::new(span),
            }),
            Expr::Tuple { items, meta, .. } => self.rewrite_expr_tuple(items, meta),
            Expr::FnPlaceholder {
                body,
                stem,
                state,
                meta,
                ..
            } => self.rewrite_expr_fn_placeholder(body, stem, state, meta),
            Expr::OpChain { kind, meta, .. } => self.rewrite_expr_op_chain(kind, meta),
            Expr::RecCall {
                callee, args, meta, ..
            } => self.rewrite_expr_rec_call(callee, args, meta),
            Expr::LabelValue { labels, meta, .. } => self.rewrite_expr_label_value(labels, meta),
            Expr::RowLet { meta, .. } => Err(crate::error::Error::parse(
                meta.span,
                "row-let syntax is not part of Kio'. The `kio-prime` binary only accepts Kio'; \
                 use the `kio` binary to compile this program as full Kio.",
            )),
            Expr::Elaborator {
                kind, call, meta, ..
            } => self.rewrite_expr_elaborator(kind, call, meta),
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match ext {},
            Expr::UserElaborator {
                name, args, meta, ..
            } => self.rewrite_expr_user_elaborator(name, args, meta),
            Expr::BlockCall { meta, .. } => Err(Error::parse(
                meta.span,
                "trailing blocks are not part of Kio'; use `kio` to compile full Kio source",
            )),
            Expr::Ufcs {
                receiver,
                callee_segments,
                callee_span,
                args,
                flavor,
                bang,
                meta,
                ..
            } => self.rewrite_expr_ufcs(SurfaceUfcs {
                receiver,
                callee_segments,
                callee_span,
                args,
                flavor,
                bang,
                meta,
            }),
        }
    }

    fn walk_type(&mut self, ty: Type<Surface>) -> Result<Type<Prime>, Error> {
        match ty {
            Type::Path {
                segments,
                args,
                meta: Meta { span, .. },
            } => {
                let args: Vec<Type<Prime>> = args
                    .into_iter()
                    .map(|a| self.walk_type(a))
                    .collect::<Result<_, _>>()?;
                // Higher-kinded type application is the ordinary
                // `Type::Path` with args — `F(A)` for a kind-`*→*`
                // binder, `Either(String)` for a partially-applied
                // newtype. The kind discipline (typer post-pass)
                // decides admissibility.
                Ok(Type::synth_path_segments(segments, args, span))
            }
            Type::Unit {
                meta: Meta { span, .. },
            } => Ok(Type::Unit {
                meta: Meta::new(span),
            }),
            Type::Bottom {
                meta: Meta { span, .. },
            } => Ok(Type::Bottom {
                meta: Meta::new(span),
            }),
            Type::Function {
                param,
                ret,
                meta: Meta { span, .. },
                abi_arity,
                ..
            } => Ok(Type::Function {
                param: Box::new(self.walk_type(*param)?),
                ret: Box::new(self.walk_type(*ret)?),
                meta: Meta::new(span),
                abi_arity,
                caps: (),
            }),
            Type::Product {
                left,
                right,
                meta: Meta { span, .. },
            } => Ok(Type::Product {
                left: Box::new(self.walk_type(*left)?),
                right: Box::new(self.walk_type(*right)?),
                meta: Meta::new(span),
            }),
            Type::Sum {
                left,
                right,
                meta: Meta { span, .. },
            } => Ok(Type::Sum {
                left: Box::new(self.walk_type(*left)?),
                right: Box::new(self.walk_type(*right)?),
                meta: Meta::new(span),
            }),
            Type::Forall {
                param,
                body,
                meta: Meta { span, .. },
            } => Ok(Type::Forall {
                param,
                body: Box::new(self.walk_type(*body)?),
                meta: Meta::new(span),
            }),
            Type::LabelSugar { labels, meta, .. } => self.rewrite_type_label_sugar(labels, meta),
            Type::Infer { meta, .. } => self.rewrite_type_infer(meta),
        }
    }
}

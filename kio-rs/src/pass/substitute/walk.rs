//! Lowered → PrePrime walker trait.
//!
//! The [`LoweredToPrePrime`] trait pairs every Lowered AST variant
//! with a `visit_*` method that converts it to its PrePrime
//! counterpart. Variants that are inhabited at PrePrime (`Path`,
//! `Call`, `FnExpr`, literals, `Type::Path`, `Type::Function`,
//! `Item::FnDef`, …) have default implementations that
//! clone-and-recurse via `self.walk_*`. Variants that are
//! uninhabited at PrePrime (the eight `ExprElab`-bearing arms on the
//! Expr side, `Type::Infer` on the Type side, `Item::Equiv` on the
//! Item side) have **no default** — the implementer must provide
//! one via a `rewrite_*` method.
//!
//! The Lowered phase has already stripped the early surface-only
//! variants (`Tuple`, `FnPlaceholder`, `LabelValue`, `LabelSugar`,
//! `Labels`, `Op`, and `LiteralAlias`), so
//! those variants discharge via `match *ext {}` in the
//! dispatchers and never reach a method.
//!
//! `walk_item` returns `Vec<Item<PrePrime>>` so an implementer may drop
//! or expand items (e.g., [`crate::pass::substitute`] drops `Item::Equiv`
//! since equiv declarations have no execution semantics in Kio').
//!
//! Inputs are taken by reference; the trait clones the per-node
//! data needed at PrePrime. This mirrors substitute's existing API —
//! callers may keep the checked Lowered package and its boundary-local records
//! while preparing evaluator inputs. The production build pipeline sends
//! backend emission only the substituted, standalone-validated Prime artifact
//! (or a later IR derived from it).

use super::PrePrime;
use crate::ast::{
    CallArg, Equiv, Expr, FnDef, HostFn, HostFnParam, HostFnValueParam, HostType, Item, Lowered,
    Meta, Module, Newtype, PackageFile, Param, RoleShape, Signature, SignatureParam, Type,
    TypeAlias, convert_meta, convert_type_member,
};
use crate::error::Error;
use crate::span::Span;

/// Walker over `Lowered → PrePrime`. Methods named `visit_*` have
/// default clone-and-recurse implementations (the variant survives
/// into PrePrime, and the default rebuilds it). Methods named
/// `rewrite_*` have no default — they handle variants that are
/// uninhabited at PrePrime, and the implementer must provide a rule
/// for each (typically: substitute the typer's recorded
/// elaboration, or drop the item).
pub(super) trait LoweredToPrePrime {
    fn completed_source_binding(&self, _source: &Expr<Lowered>) -> Option<Expr<PrePrime>> {
        None
    }

    // -------------------------------------------------------------
    // Rewrite hooks (no defaults).
    // -------------------------------------------------------------

    fn rewrite_expr_elaborator(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error>;

    fn rewrite_expr_rec_order(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error>;
    fn rewrite_expr_rec_quote(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error>;

    fn rewrite_expr_user_elaborator(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error>;

    fn rewrite_expr_ufcs(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error>;

    fn rewrite_type_infer(&mut self, meta: &Meta<Lowered>) -> Result<Type<PrePrime>, Error>;

    /// `Item::Equiv` may be dropped (return `Ok(vec![])`),
    /// transformed (return `Ok(vec![item])`), or expanded
    /// (return `Ok(vec![item1, item2, …])`); the
    /// [`Self::walk_module`] / [`Self::walk_package_file`]
    /// dispatchers flat-map the results.
    fn rewrite_item_equiv(&mut self, e: &Equiv<Lowered>) -> Result<Vec<Item<PrePrime>>, Error>;

    fn rewrite_item_elaborator(
        &mut self,
        e: &crate::ast::UserElaboratorDef<Lowered>,
    ) -> Result<Vec<Item<PrePrime>>, Error>;

    // -------------------------------------------------------------
    // Per-variant visit methods (default-recurse).
    //
    // The dispatchers below call these for each variant that is
    // inhabited at PrePrime. Implementers can override individual
    // visit_* methods to customize a variant's handling without
    // touching the dispatcher (e.g., substitute's Call arm has
    // canonical-slot infer-arg insertion that overrides
    // visit_expr_call). A dispatcher routes each variant to its own
    // visit_*, so the let-else at a method's head can fail only if a
    // dispatcher routed the wrong variant — a bug in this file, hence
    // the bare `unreachable!()`.
    // -------------------------------------------------------------

    fn visit_expr_path(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
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
            segments: segments.clone(),
            meta: convert_meta(meta),
            ext: (),
        })
    }

    fn visit_expr_call(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::Call {
            callee, args, meta, ..
        } = e
        else {
            unreachable!()
        };
        let callee = self.walk_expr(callee)?;
        let args: Vec<CallArg<PrePrime>> = args
            .iter()
            .map(|a| self.walk_call_arg(a))
            .collect::<Result<_, _>>()?;
        Ok(Expr::synth_call(callee, args, meta.span))
    }

    fn visit_expr_fn(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps: _,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                self.walk_fn_signature_params(&sig.params)?,
                sig.groups.clone(),
            ),
            // Kio' `FnExpr` carries the optional `-> R` return-type
            // annotation, same as the surface form (per `specs/prime.md`
            // § Grammar): a *fully-concrete* annotation is load-bearing
            // for a `let`-bound lambda, whose type the typer cannot
            // otherwise synthesize. A partial annotation containing any
            // `_` (outermost `-> _` or nested) has no recorded
            // resolution at substitute time, so it drops here.
            ret_ty: match ret_ty {
                Some(t) if !crate::pass::typecheck_core::type_contains_infer(t) => {
                    Some(self.walk_type(t)?)
                }
                _ => None,
            },
            body: Box::new(self.walk_expr(body)?),
            meta: convert_meta(meta),
            caps: (),
        })
    }

    fn visit_expr_let(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern: (),
            value,
            body,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Let {
            occurrence: Default::default(),
            name: name.clone(),
            name_span: *name_span,
            ty: match ty {
                Some(t) if !crate::pass::typecheck_core::type_contains_infer(t) => {
                    Some(self.walk_type(t)?)
                }
                _ => None,
            },
            pattern: (),
            value: Box::new(self.walk_expr(value)?),
            body: Box::new(self.walk_expr(body)?),
            meta: convert_meta(meta),
        })
    }

    fn visit_expr_seq(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(self.walk_expr(value)?),
            body: Box::new(self.walk_expr(body)?),
            meta: convert_meta(meta),
        })
    }

    fn visit_expr_unit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::Unit {
            occurrence: _,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Unit {
            occurrence: Default::default(),
            meta: convert_meta(meta),
        })
    }

    fn visit_expr_str_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::StrLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::StrLit {
            occurrence: Default::default(),
            value: value.clone(),
            annotation: self.walk_literal_annotation(
                annotation,
                RoleShape::Str,
                meta.span,
                e.occurrence().assigned_key(),
            )?,
            meta: convert_meta(meta),
        })
    }

    fn visit_expr_int_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::IntLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::IntLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: self.walk_literal_annotation(
                annotation,
                RoleShape::Int,
                meta.span,
                e.occurrence().assigned_key(),
            )?,
            meta: convert_meta(meta),
        })
    }

    fn visit_expr_float_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::FloatLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::FloatLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: self.walk_literal_annotation(
                annotation,
                RoleShape::Float,
                meta.span,
                e.occurrence().assigned_key(),
            )?,
            meta: convert_meta(meta),
        })
    }

    fn visit_expr_bool_lit(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        let Expr::BoolLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::BoolLit {
            occurrence: Default::default(),
            value: *value,
            annotation: self.walk_literal_annotation(
                annotation,
                RoleShape::Bool,
                meta.span,
                e.occurrence().assigned_key(),
            )?,
            meta: convert_meta(meta),
        })
    }

    /// Walk a literal's `(Type)` annotation to its now-mandatory Kio'
    /// form. The typer's three-tier resolution — recorded keyed by the
    /// literal's occurrence — is baked in here, so every `PrePrime` literal
    /// carries its concrete host type: `specs/grammar.md` § Kio'
    /// grammar mandates a `(Type)` on every literal, so the container
    /// is a bare `Type<PrePrime>`, not an `Option`.
    fn walk_literal_annotation(
        &mut self,
        annotation: &Option<Type<Lowered>>,
        shape: crate::ast::RoleShape,
        span: Span,
        occurrence: Option<crate::ast::ExpressionOccurrenceId>,
    ) -> Result<Type<PrePrime>, Error> {
        if let Some(resolved) = self.resolved_literal(occurrence)? {
            return Ok(resolved);
        }
        if let Some(t) = annotation
            && !matches!(t, Type::Infer { .. })
        {
            return self.walk_type(t);
        }
        if let Some(resolved) = self.resolved_literal_by_role(shape, span)? {
            return Ok(resolved);
        }
        unreachable!(
            "literal at {span:?} reached Lowered → PrePrime with no recorded resolution, \
             no concrete `(Type)` annotation, and no unique role-admitted host type; \
             the typer must pin every literal's host type before substitution"
        )
    }

    /// The typer's recorded three-tier resolution for the literal at
    /// `occurrence`, walked to `PrePrime`. Default: none — an implementer with
    /// an elaboration table (the full pipeline's `substitute`)
    /// overrides this to bake the resolved host type in.
    fn resolved_literal(
        &mut self,
        _occurrence: Option<crate::ast::ExpressionOccurrenceId>,
    ) -> Result<Option<Type<PrePrime>>, Error> {
        Ok(None)
    }

    /// Tier-3 fill-in for a bare literal the elaboration table did not
    /// record: the unique role-bearing host type its `shape` admits,
    /// from the enclosing module's env (`specs/language.md` § Literals
    /// tier 3). Default: none — an implementer with a package
    /// (the full pipeline's `substitute`) overrides this to consult it,
    /// so a Kio' literal produced from an untyped Lowered module (a
    /// compile-time-eval term, an elaborator template, an emitter test
    /// fixture) still resolves its mandatory annotation.
    fn resolved_literal_by_role(
        &mut self,
        _shape: crate::ast::RoleShape,
        _span: Span,
    ) -> Result<Option<Type<PrePrime>>, Error> {
        Ok(None)
    }

    fn visit_type_path(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Path {
            segments,
            args,
            meta,
        } = ty
        else {
            unreachable!()
        };
        let args: Vec<Type<PrePrime>> = args
            .iter()
            .map(|a| self.walk_type(a))
            .collect::<Result<_, _>>()?;
        Ok(Type::synth_path_segments(segments.clone(), args, meta.span))
    }

    fn visit_type_unit(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Unit { meta } = ty else {
            unreachable!()
        };
        Ok(Type::Unit {
            meta: convert_meta(meta),
        })
    }

    fn visit_type_bottom(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Bottom { meta } = ty else {
            unreachable!()
        };
        Ok(Type::Bottom {
            meta: convert_meta(meta),
        })
    }

    fn visit_type_function(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            ..
        } = ty
        else {
            unreachable!()
        };
        let param = self.walk_type(param)?;
        Ok(Type::Function {
            param: Box::new(param),
            ret: Box::new(self.walk_type(ret)?),
            meta: convert_meta(meta),
            abi_arity: *abi_arity,
            caps: (),
        })
    }

    fn visit_type_product(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Product { left, right, meta } = ty else {
            unreachable!()
        };
        Ok(Type::Product {
            left: Box::new(self.walk_type(left)?),
            right: Box::new(self.walk_type(right)?),
            meta: convert_meta(meta),
        })
    }

    fn visit_type_sum(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Sum { left, right, meta } = ty else {
            unreachable!()
        };
        Ok(Type::Sum {
            left: Box::new(self.walk_type(left)?),
            right: Box::new(self.walk_type(right)?),
            meta: convert_meta(meta),
        })
    }

    fn visit_type_forall(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        let Type::Forall { param, body, meta } = ty else {
            unreachable!()
        };
        Ok(Type::Forall {
            param: param.clone(),
            body: Box::new(self.walk_type(body)?),
            meta: convert_meta(meta),
        })
    }

    // -------------------------------------------------------------
    // Dispatchers and sub-walks (default impls).
    // -------------------------------------------------------------

    fn walk_module(&mut self, module: &Module<Lowered>) -> Result<Module<PrePrime>, Error> {
        let mut items: Vec<Item<PrePrime>> = Vec::with_capacity(module.items.len());
        for it in &module.items {
            items.extend(self.walk_item(it)?);
        }
        Ok(Module {
            path: module.path.clone(),
            imports: module.imports.clone(),
            items,
            meta: convert_meta(&module.meta),
            doc: module.doc.clone(),
        })
    }

    fn walk_package_file(
        &mut self,
        package: &PackageFile<Lowered>,
    ) -> Result<PackageFile<PrePrime>, Error> {
        Ok(PackageFile {
            name: package.name.clone(),
            build: package.build.clone(),
            // `bridge` is phase-independent — carried verbatim.
            bridge: package.bridge.clone(),
            meta: convert_meta(&package.meta),
        })
    }

    fn walk_item(&mut self, item: &Item<Lowered>) -> Result<Vec<Item<PrePrime>>, Error> {
        match item {
            Item::FnDef(d) => Ok(vec![Item::FnDef(self.walk_fn_def(d)?)]),
            Item::TypeAlias(a) => Ok(vec![Item::TypeAlias(self.walk_alias(a)?)]),
            Item::Newtype(d) => Ok(vec![Item::Newtype(self.walk_newtype(d)?)]),
            Item::TypeRecGroup(group) => {
                if group.deferred_rec_labels_diagnostic.is_some() {
                    unreachable!(
                        "an unmarked recursive labels declaration must be diagnosed before Lowered-to-Prime substitution"
                    );
                }
                let members = group
                    .members
                    .iter()
                    .map(|member| match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => self
                            .walk_alias(alias)
                            .map(crate::ast::TypeRecMember::TypeAlias),
                        crate::ast::TypeRecMember::Newtype(newtype) => self
                            .walk_newtype(newtype)
                            .map(crate::ast::TypeRecMember::Newtype),
                        crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Ok(vec![Item::TypeRecGroup(crate::ast::TypeRecGroup {
                    members,
                    doc: group.doc.clone(),
                    source_layout: group.source_layout.clone(),
                    rec_span: group.rec_span,
                    open_brace_span: group.open_brace_span,
                    close_brace_span: group.close_brace_span,
                    deferred_rec_labels_diagnostic: None,
                    meta: crate::ast::convert_meta(&group.meta),
                })])
            }
            Item::Equiv(e, _) => self.rewrite_item_equiv(e),
            Item::Elaborator(s, _) => self.rewrite_item_elaborator(s),
            // Host items are inhabited at every phase and pass straight
            // through (signature-type rebrand, no exprs); opaque in Kio'.
            Item::HostType(h) => Ok(vec![Item::HostType(self.walk_host_type(h)?)]),
            Item::HostFn(h) => Ok(vec![Item::HostFn(self.walk_host_fn(h)?)]),
            // Statically uninhabited at Lowered.
            Item::LiteralAlias(_, ext) => match *ext {},
            Item::Labels(_, ext) => match *ext {},
            Item::LabelForward(_, ext) => match *ext {},
            Item::Op(_, ext) => match *ext {},
            Item::VariadicOperator(_, ext) => match *ext {},
            Item::RecGroup(_, ext) => match *ext {},
        }
    }

    fn walk_fn_def(&mut self, d: &FnDef<Lowered>) -> Result<FnDef<PrePrime>, Error> {
        Ok(FnDef {
            vis: d.vis.clone(),
            purity: d.purity,
            name: d.name.clone(),
            sig: Signature::from_parts(
                self.walk_fn_def_signature_params(&d.sig.params)?,
                d.sig.groups.clone(),
            ),
            ret: self.walk_type(&d.ret)?,
            // Surface-only provenance: narrowed to `()` at PrePrime
            // via [`Phase::FnDefRetElided`].
            ret_elided: (),
            body: self.walk_expr(&d.body)?,
            meta: convert_meta(&d.meta),
            doc: d.doc.clone(),
        })
    }

    /// Walk an `TypeAlias` Lowered → PrePrime.
    fn walk_alias(&mut self, a: &TypeAlias<Lowered>) -> Result<TypeAlias<PrePrime>, Error> {
        Ok(TypeAlias {
            vis: a.vis.clone(),
            name: a.name.clone(),
            name_span: a.name_span,
            type_params: a.type_params.clone(),
            body: self.walk_type(&a.body)?,
            meta: convert_meta(&a.meta),
            editable_span: None,
            doc: a.doc.clone(),
        })
    }

    fn walk_newtype(&mut self, d: &Newtype<Lowered>) -> Result<Newtype<PrePrime>, Error> {
        // `existential_params` is preserved across the Lowered →
        // PrePrime boundary as-is. The typer's `newtype_member_scheme`
        // extends both constructor and projector schemes with the
        // newtype's existentials as additional type-params.
        Ok(Newtype {
            vis: d.vis.clone(),
            rec_span: d.rec_span,
            name: d.name.clone(),
            name_span: d.name_span,
            type_params: d.type_params.clone(),
            existential_params: d.existential_params.clone(),
            payload: self.walk_type(&d.payload)?,
            constructor: convert_type_member(&d.constructor),
            projector: convert_type_member(&d.projector),
            meta: convert_meta(&d.meta),
            editable_span: None,
            doc: d.doc.clone(),
        })
    }

    fn walk_host_type(&mut self, h: &HostType<Lowered>) -> Result<HostType<PrePrime>, Error> {
        Ok(HostType {
            name: h.name.clone(),
            type_params: h.type_params.clone(),
            role: h.role,
            owned: h.owned,
            meta: convert_meta(&h.meta),
            doc: h.doc.clone(),
        })
    }

    fn walk_host_fn(&mut self, h: &HostFn<Lowered>) -> Result<HostFn<PrePrime>, Error> {
        let params: Vec<HostFnParam<PrePrime>> = h
            .params
            .iter()
            .map(|p| self.walk_host_fn_param(p))
            .collect::<Result<_, _>>()?;
        Ok(HostFn {
            name: h.name.clone(),
            params,
            param_groups: h.param_groups.clone(),
            ret: self.walk_type(&h.ret)?,
            meta: convert_meta(&h.meta),
            doc: h.doc.clone(),
        })
    }

    fn walk_host_fn_param(
        &mut self,
        p: &HostFnParam<Lowered>,
    ) -> Result<HostFnParam<PrePrime>, Error> {
        Ok(match p {
            HostFnParam::Type(tp) => HostFnParam::Type(tp.clone()),
            HostFnParam::Value(v) => HostFnParam::Value(HostFnValueParam {
                name: v.name.clone(),
                ty: self.walk_type(&v.ty)?,
                meta: convert_meta(&v.meta),
            }),
        })
    }

    /// Walk a top-level signature's params. `fn` / `equiv` carry
    /// mandatory annotations; these flow through
    /// [`Self::walk_fn_def_signature_param`].
    fn walk_fn_def_signature_params(
        &mut self,
        params: &[SignatureParam<Lowered>],
    ) -> Result<Vec<SignatureParam<PrePrime>>, Error> {
        params
            .iter()
            .map(|p| self.walk_fn_def_signature_param(p))
            .collect()
    }

    fn walk_fn_def_signature_param(
        &mut self,
        p: &SignatureParam<Lowered>,
    ) -> Result<SignatureParam<PrePrime>, Error> {
        Ok(match p {
            SignatureParam::Type(tp) => SignatureParam::Type(tp.clone()),
            SignatureParam::Value(v) => SignatureParam::Value(self.walk_param(v)?),
        })
    }

    fn walk_param(&mut self, p: &Param<Lowered>) -> Result<Param<PrePrime>, Error> {
        Ok(Param {
            name: p.name.clone(),
            ty: p.ty.as_ref().map(|t| self.walk_type(t)).transpose()?,
            pattern: (),
            meta: convert_meta(&p.meta),
        })
    }

    /// Walk an `Expr::FnExpr` signature's params. Kio' `FnExpr` params
    /// carry the optional `: T` annotation, same as the surface form
    /// (per `specs/prime.md` § Grammar): a *concrete* annotation is
    /// load-bearing for a `let`-bound lambda, whose type the typer
    /// cannot otherwise synthesize. A `: _` annotation is equivalent to
    /// a bare parameter and drops (see `walk_fn_signature_param`).
    fn walk_fn_signature_params(
        &mut self,
        params: &[SignatureParam<Lowered>],
    ) -> Result<Vec<SignatureParam<PrePrime>>, Error> {
        params
            .iter()
            .map(|p| self.walk_fn_signature_param(p))
            .collect()
    }

    fn walk_fn_signature_param(
        &mut self,
        p: &SignatureParam<Lowered>,
    ) -> Result<SignatureParam<PrePrime>, Error> {
        Ok(match p {
            SignatureParam::Type(tp) => SignatureParam::Type(tp.clone()),
            SignatureParam::Value(v) => SignatureParam::Value(Param {
                name: v.name.clone(),
                // Concrete annotations survive. A partial annotation
                // has already constrained the expected slot but has no
                // independent Kio' spelling to preserve.
                ty: match &v.ty {
                    Some(t) if !crate::pass::typecheck_core::type_contains_infer(t) => {
                        Some(self.walk_type(t)?)
                    }
                    _ => None,
                },
                pattern: (),
                meta: convert_meta(&v.meta),
            }),
        })
    }

    fn walk_call_arg(&mut self, a: &CallArg<Lowered>) -> Result<CallArg<PrePrime>, Error> {
        Ok(match a {
            CallArg::Type(t) => CallArg::Type(self.walk_type(t)?),
            CallArg::Value(e) => CallArg::Value(self.walk_expr(e)?),
        })
    }

    fn walk_expr(&mut self, e: &Expr<Lowered>) -> Result<Expr<PrePrime>, Error> {
        if let Some(binding) = self.completed_source_binding(e) {
            return Ok(binding);
        }
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Path { .. } => self.visit_expr_path(e),
            Expr::Call { .. } => self.visit_expr_call(e),
            Expr::FnExpr { .. } => self.visit_expr_fn(e),
            Expr::Let { .. } => self.visit_expr_let(e),
            Expr::Seq { .. } => self.visit_expr_seq(e),
            Expr::Unit { .. } => self.visit_expr_unit(e),
            Expr::StrLit { .. } => self.visit_expr_str_lit(e),
            Expr::IntLit { .. } => self.visit_expr_int_lit(e),
            Expr::FloatLit { .. } => self.visit_expr_float_lit(e),
            Expr::BoolLit { .. } => self.visit_expr_bool_lit(e),
            Expr::Elaborator { .. } => self.rewrite_expr_elaborator(e),
            Expr::RecOrder { .. } => self.rewrite_expr_rec_order(e),
            Expr::RecQuote { .. } => self.rewrite_expr_rec_quote(e),
            Expr::UserElaborator { .. } => self.rewrite_expr_user_elaborator(e),
            Expr::Ufcs { .. } => self.rewrite_expr_ufcs(e),
            // Statically uninhabited at Lowered (`label_elab` /
            // `desugar` stripped these at earlier boundaries).
            Expr::Tuple { ext, .. } => match *ext {},
            Expr::FnPlaceholder { ext, .. } => match *ext {},
            Expr::LabelValue { ext, .. } => match *ext {},
            Expr::RowLet { ext, .. } => match *ext {},
            Expr::OpChain { ext, .. } => match *ext {},
            Expr::RecCall { ext, .. } => match *ext {},
            // Statically uninhabited at Lowered — the enriched
            // structural variants are produced *after* this
            // Lowered → PrePrime walk, by the structural-recovery pass.
            Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
            | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
            Expr::LowHostCall { ext, .. }
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

    fn walk_type(&mut self, ty: &Type<Lowered>) -> Result<Type<PrePrime>, Error> {
        match ty {
            Type::Path { .. } => self.visit_type_path(ty),
            Type::Unit { .. } => self.visit_type_unit(ty),
            Type::Bottom { .. } => self.visit_type_bottom(ty),
            Type::Function { .. } => self.visit_type_function(ty),
            Type::Product { .. } => self.visit_type_product(ty),
            Type::Sum { .. } => self.visit_type_sum(ty),
            Type::Forall { .. } => self.visit_type_forall(ty),
            Type::Infer { meta, .. } => self.rewrite_type_infer(meta),
            Type::Goal { .. } => {
                crate::pass::typecheck_core::assert_goal_free(
                    ty,
                    "Lowered-to-PrePrime substitution",
                );
                unreachable!("goal-free assertion returned for Type::Goal")
            }
            // Statically uninhabited at Lowered.
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }
}

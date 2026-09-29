use std::collections::{HashMap, HashSet};

use crate::ast::{
    CallArg, Expr, FnDef, Item, Module, PackageFile, Prime, Signature, SignatureParam, Type,
    UncheckedPrime, convert_expr, convert_fn_def, convert_item, convert_meta, convert_type,
};
use crate::pass::typecheck_core::fresh_type_var;
use crate::pass::visit_mut::{TypecheckVisitMut, walk_type};

use super::PrePrime;

pub(super) fn finish_module(mut module: Module<PrePrime>) -> Module<Prime> {
    for item in &mut module.items {
        let Item::FnDef(def) = item else {
            continue;
        };
        materialize_fn_def(def);
    }
    Module {
        path: module.path,
        imports: module.imports,
        items: module
            .items
            .iter()
            .map(convert_item::<PrePrime, Prime>)
            .collect(),
        meta: convert_meta(&module.meta),
        doc: module.doc,
    }
}

pub(super) fn finish_module_unchecked(mut module: Module<PrePrime>) -> Module<UncheckedPrime> {
    for item in &mut module.items {
        let Item::FnDef(def) = item else {
            continue;
        };
        materialize_fn_def(def);
    }
    Module {
        path: module.path,
        imports: module.imports,
        items: module
            .items
            .iter()
            .map(convert_item::<PrePrime, UncheckedPrime>)
            .collect(),
        meta: convert_meta(&module.meta),
        doc: module.doc,
    }
}

pub(super) fn finish_package_file_unchecked(
    package_file: PackageFile<PrePrime>,
) -> PackageFile<UncheckedPrime> {
    PackageFile {
        name: package_file.name,
        build: package_file.build,
        bridge: package_file.bridge,
        meta: convert_meta(&package_file.meta),
    }
}

pub(super) fn finish_package_file(package_file: PackageFile<PrePrime>) -> PackageFile<Prime> {
    PackageFile {
        name: package_file.name,
        build: package_file.build,
        bridge: package_file.bridge,
        meta: convert_meta(&package_file.meta),
    }
}

pub(super) fn finish_expr_unchecked(expr: Expr<PrePrime>) -> Expr<UncheckedPrime> {
    let mut names = GeneratedValueNames::for_expr(&expr);
    let expr = materialize_expr(expr, &mut names);
    convert_expr::<PrePrime, UncheckedPrime>(&expr)
}

pub(super) fn finish_expr_unchecked_with_reserved_type_names(
    mut expr: Expr<PrePrime>,
    reserved_type_names: &HashSet<String>,
) -> Expr<UncheckedPrime> {
    materialize_generated_type_names(&mut expr, reserved_type_names);
    finish_expr_unchecked(expr)
}

pub(super) fn finish_type_unchecked(ty: Type<PrePrime>) -> Type<UncheckedPrime> {
    convert_type::<PrePrime, UncheckedPrime>(&ty)
}

pub(super) fn finish_fn_def_unchecked(mut def: FnDef<PrePrime>) -> FnDef<UncheckedPrime> {
    materialize_fn_def(&mut def);
    convert_fn_def::<PrePrime, UncheckedPrime>(&def)
}

fn materialize_fn_def(def: &mut FnDef<PrePrime>) {
    let span = def.body.span();
    let body = std::mem::replace(
        &mut def.body,
        Expr::Unit {
            occurrence: Default::default(),
            meta: crate::ast::Meta::new(span),
        },
    );
    def.body = materialize_function(&mut def.sig, body);
}

struct TypeNameCollector<'a>(&'a mut HashSet<String>);

impl TypecheckVisitMut<PrePrime> for TypeNameCollector<'_> {
    fn enter_type_binder(&mut self, param: &mut crate::ast::TypeParam, _scope: crate::span::Span) {
        self.0.insert(param.name.clone());
    }

    fn visit_type(&mut self, ty: &mut Type<PrePrime>) {
        if let Type::Path { segments, .. } = ty
            && let [segment] = segments.as_slice()
        {
            self.0.insert(segment.name.clone());
        }
        walk_type(self, ty);
    }
}

struct GeneratedTypeNameMaterializer {
    reserved: HashSet<String>,
    bindings: HashMap<String, Vec<String>>,
    stack: Vec<String>,
}

impl GeneratedTypeNameMaterializer {
    fn new(reserved: HashSet<String>) -> Self {
        Self {
            reserved,
            bindings: HashMap::new(),
            stack: Vec::new(),
        }
    }
}

impl TypecheckVisitMut<PrePrime> for GeneratedTypeNameMaterializer {
    fn enter_type_binder(&mut self, param: &mut crate::ast::TypeParam, _scope: crate::span::Span) {
        let source = param.name.clone();
        let materialized = if crate::normalization::is_generated_checked_term_type_name(&source) {
            let fresh = fresh_type_var("Ct", &self.reserved);
            self.reserved.insert(fresh.clone());
            fresh
        } else {
            source.clone()
        };
        param.name.clone_from(&materialized);
        self.bindings
            .entry(source.clone())
            .or_default()
            .push(materialized);
        self.stack.push(source);
    }

    fn exit_type_binder(&mut self, _param: &mut crate::ast::TypeParam, _scope: crate::span::Span) {
        let source = self
            .stack
            .pop()
            .expect("generated type-name materializer exits a live binder");
        let names = self
            .bindings
            .get_mut(&source)
            .expect("generated type-name materializer retains every entered binder");
        names.pop();
        if names.is_empty() {
            self.bindings.remove(&source);
        }
    }

    fn visit_type(&mut self, ty: &mut Type<PrePrime>) {
        if let Type::Path { segments, .. } = ty
            && let [segment] = segments.as_mut_slice()
        {
            if let Some(materialized) = self
                .bindings
                .get(segment.name.as_str())
                .and_then(|entries| entries.last())
            {
                segment.name.clone_from(materialized);
            }
            if crate::normalization::is_generated_checked_term_type_name(&segment.name) {
                unreachable!("unbound compiler-only checked-term type name reached UncheckedPrime");
            }
        }
        walk_type(self, ty);
    }
}

fn materialize_generated_type_names(
    expr: &mut Expr<PrePrime>,
    reserved_type_names: &HashSet<String>,
) {
    // A checked-term binder is deliberately unspellable until every raw term,
    // template value, and residual eta layer has joined this one replay tree.
    // Seed the ordinary Kio namespace only here, at its complete lexical owner.
    let mut reserved = reserved_type_names.clone();
    TypeNameCollector(&mut reserved).visit_expr(expr);
    GeneratedTypeNameMaterializer::new(reserved).visit_expr(expr);
}

struct GeneratedValueNames {
    used: HashSet<String>,
    next: u32,
}

impl GeneratedValueNames {
    fn for_expr(body: &Expr<PrePrime>) -> Self {
        let mut used = HashSet::new();
        collect_names(body, &mut used);
        Self { used, next: 0 }
    }

    fn for_function(sig: &Signature<PrePrime>, body: &Expr<PrePrime>) -> Self {
        let mut names = Self::for_expr(body);
        collect_signature_names(sig, &mut names.used);
        names
    }

    fn fresh(&mut self) -> String {
        loop {
            let candidate = format!("_kg{}", self.next);
            self.next += 1;
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

fn collect_signature_names(sig: &Signature<PrePrime>, out: &mut HashSet<String>) {
    for param in &sig.params {
        let name = match param {
            SignatureParam::Type(param) => &param.name,
            SignatureParam::Value(param) => &param.name,
        };
        out.insert(name.clone());
    }
}

fn collect_names(expr: &Expr<PrePrime>, out: &mut HashSet<String>) {
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => {
            if let Some(head) = segments.first() {
                out.insert(head.name.clone());
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_names(callee, out);
            for arg in args {
                if let CallArg::Value(value) = arg {
                    collect_names(value, out);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            collect_signature_names(sig, out);
            collect_names(body, out);
        }
        Expr::Let {
            name, value, body, ..
        } => {
            out.insert(name.clone());
            collect_names(value, out);
            collect_names(body, out);
        }
        Expr::Seq { value, body, .. } => {
            collect_names(value, out);
            collect_names(body, out);
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. }
        | Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. }
        | Expr::OpChain { ext, .. }
        | Expr::RecCall { ext, .. }
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

fn materialize_function(sig: &mut Signature<PrePrime>, mut body: Expr<PrePrime>) -> Expr<PrePrime> {
    let mut names = GeneratedValueNames::for_function(sig, &body);
    for param in &mut sig.params {
        let SignatureParam::Value(param) = param else {
            continue;
        };
        if !param.name.starts_with("__") {
            continue;
        }
        let old = std::mem::take(&mut param.name);
        let fresh = names.fresh();
        body = rename_bound_uses(body, &old, &fresh);
        param.name = fresh;
    }
    materialize_expr(body, &mut names)
}

fn materialize_expr(expr: Expr<PrePrime>, names: &mut GeneratedValueNames) -> Expr<PrePrime> {
    match expr {
        Expr::FnExpr {
            occurrence: _,
            mut sig,
            ret_ty,
            body,
            meta,
            caps,
        } => Expr::FnExpr {
            occurrence: Default::default(),
            body: Box::new(materialize_function(&mut sig, *body)),
            sig,
            ret_ty,
            meta,
            caps,
        },
        Expr::Let {
            occurrence: _,
            mut name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        } => {
            let value = materialize_expr(*value, names);
            let mut body = *body;
            if name.starts_with("__") {
                let fresh = names.fresh();
                body = rename_bound_uses(body, &name, &fresh);
                name = fresh;
            }
            Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern,
                value: Box::new(value),
                body: Box::new(materialize_expr(body, names)),
                meta,
            }
        }
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(materialize_expr(*value, names)),
            body: Box::new(materialize_expr(*body, names)),
            meta,
        },
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(materialize_expr(*callee, names)),
            args: args
                .into_iter()
                .map(|arg| match arg {
                    CallArg::Type(ty) => CallArg::Type(ty),
                    CallArg::Value(value) => CallArg::Value(materialize_expr(value, names)),
                })
                .collect(),
            meta,
            ext: (),
        },
        other => other,
    }
}

fn rename_bound_uses(expr: Expr<PrePrime>, old: &str, new: &str) -> Expr<PrePrime> {
    match expr {
        Expr::Path {
            occurrence: _,
            mut segments,
            meta,
            ext: _,
        } => {
            if segments.len() == 1
                && let Some(head) = segments.first_mut()
                && head.name == old
            {
                head.name = new.to_owned();
            }
            Expr::Path {
                occurrence: Default::default(),
                segments,
                meta,
                ext: (),
            }
        }
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(rename_bound_uses(*callee, old, new)),
            args: args
                .into_iter()
                .map(|arg| match arg {
                    CallArg::Type(ty) => CallArg::Type(ty),
                    CallArg::Value(value) => CallArg::Value(rename_bound_uses(value, old, new)),
                })
                .collect(),
            meta,
            ext: (),
        },
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => {
            let shadows = sig
                .params
                .iter()
                .any(|param| matches!(param, SignatureParam::Value(param) if param.name == old));
            Expr::FnExpr {
                occurrence: Default::default(),
                sig,
                ret_ty,
                body: if shadows {
                    body
                } else {
                    Box::new(rename_bound_uses(*body, old, new))
                },
                meta,
                caps,
            }
        }
        Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        } => Expr::Let {
            occurrence: Default::default(),
            value: Box::new(rename_bound_uses(*value, old, new)),
            body: if name == old {
                body
            } else {
                Box::new(rename_bound_uses(*body, old, new))
            },
            name,
            name_span,
            ty,
            pattern,
            meta,
        },
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(rename_bound_uses(*value, old, new)),
            body: Box::new(rename_bound_uses(*body, old, new)),
            meta,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Meta, Param, PathSegment, SignatureGroupKind, TypeParam};
    use crate::span::Span;

    fn span(n: u32) -> Span {
        Span::new(n, n + 1)
    }

    fn path(name: &str) -> Expr<PrePrime> {
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name.to_owned(), span(0))],
            meta: Meta::new(span(0)),
            ext: (),
        }
    }

    fn ty(name: &str) -> Type<PrePrime> {
        Type::Path {
            segments: vec![PathSegment::new(name.to_owned(), span(0))],
            args: Vec::new(),
            meta: Meta::new(span(0)),
        }
    }

    fn qualified(head: &str, tail: &str) -> Expr<PrePrime> {
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![
                PathSegment::new(head.to_owned(), span(0)),
                PathSegment::new(tail.to_owned(), span(0)),
            ],
            meta: Meta::new(span(0)),
            ext: (),
        }
    }

    fn let_(name: &str, value: Expr<PrePrime>, body: Expr<PrePrime>, n: u32) -> Expr<PrePrime> {
        Expr::Let {
            occurrence: Default::default(),
            name: name.to_owned(),
            name_span: span(n),
            ty: None,
            pattern: (),
            value: Box::new(value),
            body: Box::new(body),
            meta: Meta::new(span(n)),
        }
    }

    fn seq(value: Expr<PrePrime>, body: Expr<PrePrime>) -> Expr<PrePrime> {
        Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(value),
            body: Box::new(body),
            meta: Meta::new(span(0)),
        }
    }

    fn lambda(name: &str, body: Expr<PrePrime>) -> Expr<PrePrime> {
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![SignatureParam::Value(Param {
                name: name.to_owned(),
                ty: None,
                pattern: (),
                meta: Meta::new(span(0)),
            })]),
            ret_ty: None,
            body: Box::new(body),
            meta: Meta::new(span(0)),
            caps: (),
        }
    }

    fn path_is(expr: &Expr<PrePrime>, name: &str) -> bool {
        matches!(expr, Expr::Path { segments, .. } if segments.len() == 1 && segments[0].name == name)
    }

    #[test]
    fn allocator_seeds_the_complete_lexical_owner() {
        let mut sig = Signature::new(vec![SignatureParam::Value(Param {
            name: "_kg0".to_owned(),
            ty: None,
            pattern: (),
            meta: Meta::new(span(1)),
        })]);
        let body = let_(
            "__generated__",
            path("_kg0"),
            seq(
                qualified("_kg1", "member"),
                let_(
                    "_kg3",
                    path("_kg0"),
                    lambda("_kg2", path("__generated__")),
                    2,
                ),
            ),
            1,
        );

        let materialized = materialize_function(&mut sig, body);
        let Expr::Let { name, body, .. } = &materialized else {
            panic!("expected generated let: {materialized:?}");
        };
        assert_eq!(name, "_kg4");
        let Expr::Seq { body, .. } = body.as_ref() else {
            panic!("expected sequence: {body:?}");
        };
        let Expr::Let { body, .. } = body.as_ref() else {
            panic!("expected nested let: {body:?}");
        };
        let Expr::FnExpr { body, .. } = body.as_ref() else {
            panic!("expected nested function: {body:?}");
        };
        assert!(path_is(body, "_kg4"));
    }

    #[test]
    fn materialization_preserves_nested_shadowing() {
        let mut sig = Signature::new(Vec::new());
        let body = let_(
            "__same__",
            path("seed"),
            let_("__same__", path("__same__"), path("__same__"), 2),
            1,
        );

        let materialized = materialize_function(&mut sig, body);
        let Expr::Let {
            name: outer, body, ..
        } = &materialized
        else {
            panic!("expected outer let: {materialized:?}");
        };
        let Expr::Let {
            name: inner,
            value,
            body,
            ..
        } = body.as_ref()
        else {
            panic!("expected inner let: {body:?}");
        };
        assert_ne!(outer, inner);
        assert!(path_is(value, outer));
        assert!(path_is(body, inner));
    }

    #[test]
    fn nested_functions_own_their_deterministic_supply() {
        let mut sig = Signature::new(vec![SignatureParam::Value(Param {
            name: "_kg0".to_owned(),
            ty: None,
            pattern: (),
            meta: Meta::new(span(1)),
        })]);
        let body = lambda("__generated__", path("__generated__"));

        let first = materialize_function(&mut sig, body.clone());
        let mut repeated_sig = Signature::new(vec![SignatureParam::Value(Param {
            name: "_kg0".to_owned(),
            ty: None,
            pattern: (),
            meta: Meta::new(span(1)),
        })]);
        let repeated = materialize_function(&mut repeated_sig, body);
        assert_eq!(first, repeated);
        let Expr::FnExpr { sig, body, .. } = first else {
            panic!("expected nested function");
        };
        let SignatureParam::Value(param) = &sig.params[0] else {
            panic!("expected value parameter");
        };
        assert_eq!(param.name, "_kg0");
        assert!(path_is(&body, "_kg0"));
    }

    #[test]
    fn unchecked_exit_consumes_reserved_binders_after_whole_root_seeding() {
        let expr = seq(
            path("_kg0"),
            let_("__generated__", path("seed"), path("__generated__"), 1),
        );

        let finished = finish_expr_unchecked(expr);
        let Expr::Seq { body, .. } = finished else {
            panic!("expected sequence");
        };
        let Expr::Let { name, body, .. } = body.as_ref() else {
            panic!("expected generated let: {body:?}");
        };
        assert_eq!(name, "_kg1");
        assert!(
            matches!(body.as_ref(), Expr::Path { segments, .. } if segments.len() == 1 && segments[0].name == "_kg1")
        );
    }

    #[test]
    fn replay_exit_materializes_generated_type_names_without_capture() {
        let marker = "_ct_type_n41";
        let inner = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                vec![
                    SignatureParam::Type(TypeParam {
                        name: marker.to_owned(),
                        span: span(2),
                        kind: None,
                    }),
                    SignatureParam::Value(Param {
                        name: "inner".to_owned(),
                        ty: Some(ty(marker)),
                        pattern: (),
                        meta: Meta::new(span(2)),
                    }),
                ],
                vec![
                    SignatureGroupKind::Type { len: 1 },
                    SignatureGroupKind::Value { len: 1 },
                ],
            ),
            ret_ty: Some(ty(marker)),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span(2)),
            }),
            meta: Meta::new(span(2)),
            caps: (),
        };
        let after_inner = Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(path("consume")),
            args: vec![
                CallArg::Type(ty(marker)),
                CallArg::Value(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(span(3)),
                }),
            ],
            meta: Meta::new(span(3)),
            ext: (),
        };
        let outer = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                vec![
                    SignatureParam::Type(TypeParam {
                        name: marker.to_owned(),
                        span: span(1),
                        kind: None,
                    }),
                    SignatureParam::Value(Param {
                        name: "outer".to_owned(),
                        ty: Some(ty(marker)),
                        pattern: (),
                        meta: Meta::new(span(1)),
                    }),
                    SignatureParam::Value(Param {
                        name: "free".to_owned(),
                        ty: Some(ty("Ct_n2")),
                        pattern: (),
                        meta: Meta::new(span(1)),
                    }),
                ],
                vec![
                    SignatureGroupKind::Type { len: 1 },
                    SignatureGroupKind::Value { len: 2 },
                ],
            ),
            ret_ty: Some(ty(marker)),
            body: Box::new(seq(inner, after_inner)),
            meta: Meta::new(span(1)),
            caps: (),
        };

        let finished = finish_expr_unchecked_with_reserved_type_names(
            outer,
            &HashSet::from(["Ct".to_owned()]),
        );
        let Expr::FnExpr {
            sig,
            ret_ty: Some(ret_ty),
            body,
            ..
        } = finished
        else {
            panic!("expected outer function")
        };
        let SignatureParam::Type(outer_type) = &sig.params[0] else {
            panic!("expected outer type parameter")
        };
        assert_eq!(outer_type.name, "Ct_n3");
        assert!(
            matches!(&sig.params[1], SignatureParam::Value(param) if matches!(param.ty.as_ref(), Some(Type::Path { segments, .. }) if segments[0].name == "Ct_n3"))
        );
        assert!(
            matches!(&sig.params[2], SignatureParam::Value(param) if matches!(param.ty.as_ref(), Some(Type::Path { segments, .. }) if segments[0].name == "Ct_n2"))
        );
        assert!(matches!(ret_ty, Type::Path { segments, .. } if segments[0].name == "Ct_n3"));

        let Expr::Seq { value, body, .. } = body.as_ref() else {
            panic!("expected nested function followed by an outer use")
        };
        let Expr::FnExpr {
            sig,
            ret_ty: Some(ret_ty),
            ..
        } = value.as_ref()
        else {
            panic!("expected inner function")
        };
        let SignatureParam::Type(inner_type) = &sig.params[0] else {
            panic!("expected inner type parameter")
        };
        assert_eq!(inner_type.name, "Ct_n4");
        assert!(
            matches!(&sig.params[1], SignatureParam::Value(param) if matches!(param.ty.as_ref(), Some(Type::Path { segments, .. }) if segments[0].name == "Ct_n4"))
        );
        assert!(matches!(ret_ty, Type::Path { segments, .. } if segments[0].name == "Ct_n4"));
        let Expr::Call { args, .. } = body.as_ref() else {
            panic!("expected use after inner shadow")
        };
        assert!(
            matches!(&args[0], CallArg::Type(Type::Path { segments, .. }) if segments[0].name == "Ct_n3")
        );
    }

    #[test]
    fn replay_exit_materializes_generated_forall_type_name() {
        let marker = "_ct_forall_n7";
        let expr = Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(path("consume")),
            args: vec![
                CallArg::Type(Type::Forall {
                    param: TypeParam {
                        name: marker.to_owned(),
                        span: span(1),
                        kind: None,
                    },
                    body: Box::new(ty(marker)),
                    meta: Meta::new(span(1)),
                }),
                CallArg::Type(Type::Path {
                    segments: vec![
                        PathSegment::new("_ct_type_n0".to_owned(), span(1)),
                        PathSegment::new("Foo".to_owned(), span(1)),
                    ],
                    args: Vec::new(),
                    meta: Meta::new(span(1)),
                }),
            ],
            meta: Meta::new(span(1)),
            ext: (),
        };

        let finished = finish_expr_unchecked_with_reserved_type_names(expr, &HashSet::new());
        let Expr::Call { args, .. } = finished else {
            panic!("expected call")
        };
        let CallArg::Type(Type::Forall { param, body, .. }) = &args[0] else {
            panic!("expected forall argument")
        };
        assert_eq!(param.name, "Ct");
        assert!(matches!(body.as_ref(), Type::Path { segments, .. } if segments[0].name == "Ct"));
        assert!(matches!(
            &args[1],
            CallArg::Type(Type::Path { segments, .. })
                if segments[0].name == "_ct_type_n0" && segments[1].name == "Foo"
        ));
    }
}

//! Canonical statement-spine normalization for checked Kio'.
//!
//! The AST can represent `Let` / `Seq` nodes in expression-only
//! positions even though Kio' source cannot. Surface elaboration can
//! produce those shapes in a let RHS or call operand. This pass moves
//! every such chain onto the surrounding statement spine in checked
//! module/backend artifacts, preserving left-to-right evaluation.
//!
//! Moving a chain widens its binders' lexical scope. The rewrite uses
//! a per-function, source-name-seeded supply and alpha-renames a
//! widened binder whenever the added continuation contains a free
//! path with the same head. Call-prelude temporaries use the same
//! collision set, so legal user names such as `_kp0` cannot be
//! captured when the canonical form is rendered and parsed again.

use crate::ast::{
    CallArg, Expr, Item, Meta, Module, PathSegment, Prime, Signature, SignatureParam,
};
use crate::pass::resolve::Package;
use crate::span::Span;
use std::collections::HashSet;

/// Normalize every function body in `module` to the canonical Kio'
/// statement spine. Each function owns its name supply: unrelated
/// declarations elsewhere in the module cannot perturb its output.
pub(crate) fn canonicalize_module(module: &mut Module<Prime>) {
    for item in &mut module.items {
        let Item::FnDef(def) = item else {
            continue;
        };
        let span = def.body.span();
        let body = std::mem::replace(
            &mut def.body,
            Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            },
        );
        let mut names = FreshNames::for_fn(&def.sig, &body);
        def.body = canonicalize_expr(body, &mut names);
    }
}

pub(crate) fn canonicalize_package(package: &mut Package<Prime>) {
    for entry in package.modules_mut() {
        canonicalize_module(&mut entry.module);
    }
}

struct FreshNames {
    used: HashSet<String>,
    call_index: u32,
}

impl FreshNames {
    fn for_fn(sig: &Signature<Prime>, body: &Expr<Prime>) -> Self {
        let mut used = HashSet::new();
        collect_signature_names(sig, &mut used);
        collect_names(body, &mut used);
        Self {
            used,
            call_index: 0,
        }
    }

    fn call_temp(&mut self) -> String {
        loop {
            let candidate = format!("_kp{}", self.call_index);
            self.call_index += 1;
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    fn association_binder(&mut self, span: Span) -> String {
        for probe in 0u32.. {
            let candidate = format!("_kpa_s{}_e{}_p{}", span.start, span.end, probe);
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
        }
        unreachable!("u32 association-name probe space exhausted")
    }
}

fn collect_signature_names(sig: &Signature<Prime>, out: &mut HashSet<String>) {
    for param in &sig.params {
        let name = match param {
            SignatureParam::Type(param) => &param.name,
            SignatureParam::Value(param) => &param.name,
        };
        out.insert(name.clone());
    }
}

fn collect_names(e: &Expr<Prime>, out: &mut HashSet<String>) {
    match e {
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

fn canonicalize_expr(e: Expr<Prime>, names: &mut FreshNames) -> Expr<Prime> {
    match e {
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => {
            let mut nested_names = FreshNames::for_fn(&sig, &body);
            Expr::FnExpr {
                occurrence: Default::default(),
                sig,
                ret_ty,
                body: Box::new(canonicalize_expr(*body, &mut nested_names)),
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
        } => {
            let value = canonicalize_expr(*value, names);
            let body = canonicalize_expr(*body, names);
            if !is_chain(&value) {
                return Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span,
                    ty,
                    pattern,
                    value: Box::new(value),
                    body: Box::new(body),
                    meta,
                };
            }
            let mut continuation_free = free_path_heads(&body);
            continuation_free.remove(&name);
            splice_chain(value, &continuation_free, names, move |tail| Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern,
                value: Box::new(tail),
                body: Box::new(body),
                meta,
            })
        }
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => {
            let value = canonicalize_expr(*value, names);
            let body = canonicalize_expr(*body, names);
            if !is_chain(&value) {
                return Expr::Seq {
                    occurrence: Default::default(),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta,
                };
            }
            let continuation_free = free_path_heads(&body);
            splice_chain(value, &continuation_free, names, move |tail| Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(tail),
                body: Box::new(body),
                meta,
            })
        }
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext: _,
        } => {
            let callee = canonicalize_expr(*callee, names);
            let args: Vec<CallArg<Prime>> = args
                .into_iter()
                .map(|arg| match arg {
                    CallArg::Type(ty) => CallArg::Type(ty),
                    CallArg::Value(value) => CallArg::Value(canonicalize_expr(value, names)),
                })
                .collect();
            let hoisting = is_chain(&callee)
                || args
                    .iter()
                    .any(|arg| matches!(arg, CallArg::Value(value) if is_chain(value)));
            if !hoisting {
                return Expr::Call {
                    occurrence: Default::default(),
                    callee: Box::new(callee),
                    args,
                    meta,
                    ext: (),
                };
            }

            let mut prelude = Vec::new();
            let callee = bind_if_effectful(callee, &mut prelude, names);
            let args = args
                .into_iter()
                .map(|arg| match arg {
                    CallArg::Type(ty) => CallArg::Type(ty),
                    CallArg::Value(value) => {
                        CallArg::Value(bind_if_effectful(value, &mut prelude, names))
                    }
                })
                .collect();
            let mut result = Expr::Call {
                occurrence: Default::default(),
                callee: Box::new(callee),
                args,
                meta,
                ext: (),
            };
            for (name, value) in prelude.into_iter().rev() {
                let span = value.span();
                let continuation_free = free_path_heads(&result);
                result = splice_chain(value, &continuation_free, names, move |tail| Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span: span,
                    ty: None,
                    pattern: (),
                    value: Box::new(tail),
                    body: Box::new(result),
                    meta: Meta::new(span),
                });
            }
            result
        }
        other => other,
    }
}

fn is_chain(e: &Expr<Prime>) -> bool {
    matches!(e, Expr::Let { .. } | Expr::Seq { .. })
}

fn bind_if_effectful(
    e: Expr<Prime>,
    prelude: &mut Vec<(String, Expr<Prime>)>,
    names: &mut FreshNames,
) -> Expr<Prime> {
    match e {
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. }
        | Expr::FnExpr { .. } => e,
        effectful => {
            let span = effectful.span();
            let name = names.call_temp();
            prelude.push((name.clone(), effectful));
            path_local(&name, span)
        }
    }
}

fn splice_chain<K>(
    e: Expr<Prime>,
    continuation_free: &HashSet<String>,
    names: &mut FreshNames,
    finish: K,
) -> Expr<Prime>
where
    K: FnOnce(Expr<Prime>) -> Expr<Prime>,
{
    match e {
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
            let mut body = *body;
            if continuation_free.contains(&name) {
                let fresh = names.association_binder(name_span);
                body = rename_bound_uses(body, &name, &fresh);
                name = fresh;
            }
            Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern,
                value,
                body: Box::new(splice_chain(body, continuation_free, names, finish)),
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
            value,
            body: Box::new(splice_chain(*body, continuation_free, names, finish)),
            meta,
        },
        tail => finish(tail),
    }
}

/// Rename references owned by one let binder inside its original
/// lexical body. A same-name nested binder owns its body, but not its
/// RHS, so the walk stops only after visiting that RHS.
fn rename_bound_uses(e: Expr<Prime>, old: &str, new: &str) -> Expr<Prime> {
    match e {
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

fn free_path_heads(e: &Expr<Prime>) -> HashSet<String> {
    let mut free = HashSet::new();
    collect_free_path_heads(e, &HashSet::new(), &mut free);
    free
}

fn collect_free_path_heads(e: &Expr<Prime>, bound: &HashSet<String>, free: &mut HashSet<String>) {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => {
            if let Some(head) = segments.first()
                && !bound.contains(&head.name)
            {
                free.insert(head.name.clone());
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_free_path_heads(callee, bound, free);
            for arg in args {
                if let CallArg::Value(value) = arg {
                    collect_free_path_heads(value, bound, free);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            let mut nested = bound.clone();
            for param in &sig.params {
                if let SignatureParam::Value(param) = param {
                    nested.insert(param.name.clone());
                }
            }
            collect_free_path_heads(body, &nested, free);
        }
        Expr::Let {
            name, value, body, ..
        } => {
            collect_free_path_heads(value, bound, free);
            let mut nested = bound.clone();
            nested.insert(name.clone());
            collect_free_path_heads(body, &nested, free);
        }
        Expr::Seq { value, body, .. } => {
            collect_free_path_heads(value, bound, free);
            collect_free_path_heads(body, bound, free);
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

fn path_local(name: &str, span: Span) -> Expr<Prime> {
    Expr::Path {
        occurrence: Default::default(),
        segments: vec![PathSegment {
            name: name.to_owned(),
            span,
        }],
        meta: Meta::new(span),
        ext: (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(n: u32) -> Span {
        Span::new(n, n + 1)
    }

    fn path(name: &str) -> Expr<Prime> {
        path_local(name, span(1))
    }

    fn qualified(head: &str, member: &str) -> Expr<Prime> {
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![
                PathSegment {
                    name: head.to_owned(),
                    span: span(1),
                },
                PathSegment {
                    name: member.to_owned(),
                    span: span(2),
                },
            ],
            meta: Meta::new(span(1)),
            ext: (),
        }
    }

    fn call(name: &str) -> Expr<Prime> {
        call_with_args(name, Vec::new())
    }

    fn call_with_args(name: &str, args: Vec<Expr<Prime>>) -> Expr<Prime> {
        Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(path(name)),
            args: args.into_iter().map(CallArg::Value).collect(),
            meta: Meta::new(span(3)),
            ext: (),
        }
    }

    fn let_(name: &str, value: Expr<Prime>, body: Expr<Prime>, at: u32) -> Expr<Prime> {
        Expr::Let {
            occurrence: Default::default(),
            name: name.to_owned(),
            name_span: span(at),
            ty: None,
            pattern: (),
            value: Box::new(value),
            body: Box::new(body),
            meta: Meta::new(span(at)),
        }
    }

    fn seq(value: Expr<Prime>, body: Expr<Prime>) -> Expr<Prime> {
        Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(value),
            body: Box::new(body),
            meta: Meta::new(span(4)),
        }
    }

    fn lambda(param: Option<&str>, body: Expr<Prime>) -> Expr<Prime> {
        let params = param
            .map(|name| {
                vec![SignatureParam::Value(crate::ast::Param {
                    name: name.to_owned(),
                    ty: None,
                    pattern: (),
                    meta: Meta::new(span(5)),
                })]
            })
            .unwrap_or_default();
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(params),
            ret_ty: None,
            body: Box::new(body),
            meta: Meta::new(span(5)),
            caps: (),
        }
    }

    fn canonical(body: Expr<Prime>) -> Expr<Prime> {
        let sig = Signature::new(Vec::new());
        let mut names = FreshNames::for_fn(&sig, &body);
        canonicalize_expr(body, &mut names)
    }

    fn path_is(e: &Expr<Prime>, expected: &str) -> bool {
        matches!(e, Expr::Path { segments, .. } if segments.len() == 1 && segments[0].name == expected)
    }

    fn node_count(e: &Expr<Prime>) -> usize {
        match e {
            Expr::Call { callee, args, .. } => {
                1 + node_count(callee)
                    + args
                        .iter()
                        .map(|arg| match arg {
                            CallArg::Value(value) => node_count(value),
                            CallArg::Type(_) => 0,
                        })
                        .sum::<usize>()
            }
            Expr::FnExpr { body, .. } => 1 + node_count(body),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                1 + node_count(value) + node_count(body)
            }
            _ => 1,
        }
    }

    fn call_order(e: &Expr<Prime>, out: &mut Vec<String>) {
        match e {
            Expr::Call { callee, args, .. } => {
                call_order(callee, out);
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        call_order(value, out);
                    }
                }
                if let Expr::Path { segments, .. } = callee.as_ref()
                    && segments.len() == 1
                {
                    out.push(segments[0].name.clone());
                }
            }
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                call_order(value, out);
                call_order(body, out);
            }
            Expr::FnExpr { .. } => {}
            _ => {}
        }
    }

    fn assert_canonical_positions(e: &Expr<Prime>, statement_position: bool) {
        match e {
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                assert!(
                    statement_position,
                    "chain remained in expression position: {e:?}"
                );
                assert_canonical_positions(value, false);
                assert_canonical_positions(body, true);
            }
            Expr::Call { callee, args, .. } => {
                assert_canonical_positions(callee, false);
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        assert_canonical_positions(value, false);
                    }
                }
            }
            Expr::FnExpr { body, .. } => assert_canonical_positions(body, true),
            _ => {}
        }
    }

    #[test]
    fn associates_all_let_seq_pairs_without_reordering_or_growth() {
        for outer_is_let in [false, true] {
            for inner_is_let in [false, true] {
                let inner = if inner_is_let {
                    let_("inner", call("effect_a"), call("effect_b"), 10)
                } else {
                    seq(call("effect_a"), call("effect_b"))
                };
                let original = if outer_is_let {
                    let_("outer", inner, call("effect_c"), 20)
                } else {
                    seq(inner, call("effect_c"))
                };
                let original_nodes = node_count(&original);
                let associated = canonical(original);
                let mut order = Vec::new();
                call_order(&associated, &mut order);

                assert_eq!(order, ["effect_a", "effect_b", "effect_c"]);
                assert_eq!(node_count(&associated), original_nodes);
                assert_eq!(matches!(associated, Expr::Let { .. }), inner_is_let);
                assert_canonical_positions(&associated, true);
            }
        }
    }

    #[test]
    fn associates_an_arbitrarily_long_value_chain_in_one_walk() {
        let depth = 40;
        let mut chain = path("tail");
        for _ in 0..depth {
            chain = seq(call("effect"), chain);
        }
        let associated = canonical(let_("result", chain, path("result"), 20));

        let mut cursor = &associated;
        for _ in 0..depth {
            let Expr::Seq { body, .. } = cursor else {
                panic!("whole value chain should move onto the statement spine: {cursor:?}");
            };
            cursor = body;
        }
        assert!(
            matches!(cursor, Expr::Let { name, value, .. } if name == "result" && path_is(value, "tail"))
        );
        assert_canonical_positions(&associated, true);
    }

    #[test]
    fn alpha_renames_a_widened_binder_without_capturing_deferred_code() {
        let original = seq(
            let_("collision", call("effect"), path("collision"), 30),
            lambda(None, path("collision")),
        );
        let associated = canonical(original);
        let Expr::Let {
            name, body, value, ..
        } = &associated
        else {
            panic!("inner let should lead the associated spine: {associated:?}");
        };
        assert_ne!(name, "collision");
        assert!(name.starts_with("_kpa_s30_e31_p"));
        assert!(matches!(value.as_ref(), Expr::Call { .. }));
        let Expr::Seq { value, body, .. } = body.as_ref() else {
            panic!("outer sequence should follow the associated binder: {body:?}");
        };
        assert!(path_is(value, name));
        assert!(matches!(body.as_ref(), Expr::FnExpr { body, .. } if path_is(body, "collision")));
    }

    #[test]
    fn outer_same_name_let_keeps_the_continuation_shadowed() {
        let associated = canonical(let_(
            "same",
            let_("same", call("effect"), path("same"), 30),
            path("same"),
            40,
        ));
        let Expr::Let { name, body, .. } = associated else {
            panic!("inner let should lead the associated spine");
        };
        assert_eq!(name, "same");
        assert!(
            matches!(body.as_ref(), Expr::Let { name, body, .. } if name == "same" && path_is(body, "same"))
        );
    }

    #[test]
    fn alpha_rename_stops_at_nested_let_and_lambda_shadowing() {
        let nested = let_(
            "collision",
            path("collision"),
            lambda(Some("collision"), path("collision")),
            50,
        );
        let renamed = rename_bound_uses(nested, "collision", "fresh");
        let Expr::Let { value, body, .. } = renamed else {
            panic!("expected nested let");
        };
        assert!(path_is(&value, "fresh"));
        assert!(matches!(body.as_ref(), Expr::FnExpr { body, .. } if path_is(body, "collision")));
    }

    #[test]
    fn multiple_colliding_chain_binders_receive_distinct_names() {
        let associated = canonical(seq(
            let_(
                "collision",
                call("effect_a"),
                let_("collision", call("effect_b"), path("collision"), 52),
                51,
            ),
            path("collision"),
        ));
        let Expr::Let {
            name: outer, body, ..
        } = &associated
        else {
            panic!("outer colliding binder should lead the spine: {associated:?}");
        };
        let Expr::Let {
            name: inner, body, ..
        } = body.as_ref()
        else {
            panic!("inner colliding binder should remain next: {body:?}");
        };
        assert_ne!(outer, "collision");
        assert_ne!(inner, "collision");
        assert_ne!(outer, inner);
        assert!(
            matches!(body.as_ref(), Expr::Seq { value, body, .. } if path_is(value, inner) && path_is(body, "collision"))
        );
    }

    #[test]
    fn qualified_path_heads_trigger_avoidance_but_are_not_alpha_renamed() {
        let associated = canonical(seq(
            let_("alias", call("effect"), path("alias"), 60),
            qualified("alias", "member"),
        ));
        let Expr::Let { name, body, .. } = associated else {
            panic!("inner let should lead the associated spine");
        };
        assert_ne!(name, "alias");
        assert!(
            matches!(body.as_ref(), Expr::Seq { body, .. } if matches!(body.as_ref(), Expr::Path { segments, .. } if segments.len() == 2 && segments[0].name == "alias"))
        );
    }

    #[test]
    fn call_prelude_skips_user_kp_names() {
        let canonical = canonical(call_with_args(
            "apply",
            vec![seq(call("effect_a"), call("effect_b")), path("_kp0")],
        ));
        assert!(matches!(
            &canonical,
            Expr::Seq { body, .. }
                if matches!(body.as_ref(), Expr::Let { name, .. } if name == "_kp1")
        ));
        assert!(format!("{canonical:?}").contains("_kp0"));
        let mut order = Vec::new();
        call_order(&canonical, &mut order);
        assert_eq!(order, ["effect_a", "effect_b", "apply"]);
        assert_canonical_positions(&canonical, true);
    }

    #[test]
    fn nested_function_call_temp_supply_is_lexically_owned() {
        let outer_sig = Signature::new(vec![SignatureParam::Value(crate::ast::Param {
            name: "_kp0".to_owned(),
            ty: None,
            pattern: (),
            meta: Meta::new(span(1)),
        })]);
        let body = lambda(
            None,
            call_with_args(
                "apply",
                vec![seq(call("effect_a"), call("effect_b")), path("value")],
            ),
        );
        let mut outer_names = FreshNames::for_fn(&outer_sig, &body);

        let canonical = canonicalize_expr(body, &mut outer_names);
        let Expr::FnExpr { body, .. } = canonical else {
            panic!("expected nested function");
        };
        assert!(matches!(
            body.as_ref(),
            Expr::Seq { body, .. }
                if matches!(body.as_ref(), Expr::Let { name, .. } if name == "_kp0")
        ));
    }

    #[test]
    fn call_operand_chain_binders_are_hygienic_over_later_args() {
        let canonical = canonical(call_with_args(
            "apply",
            vec![
                let_("collision", call("effect"), path("collision"), 70),
                path("collision"),
            ],
        ));
        let Expr::Let { name, body, .. } = &canonical else {
            panic!("operand chain should lead the call prelude: {canonical:?}");
        };
        assert_ne!(name, "collision");
        assert!(name.starts_with("_kpa_s70_e71_p"));
        let Expr::Let {
            name: temp,
            value,
            body,
            ..
        } = body.as_ref()
        else {
            panic!("associated operand tail should bind a call temp: {body:?}");
        };
        assert!(temp.starts_with("_kp"));
        assert!(path_is(value, name));
        let Expr::Call { args, .. } = body.as_ref() else {
            panic!("call temp should continue into the original call: {body:?}");
        };
        assert!(matches!(&args[0], CallArg::Value(value) if path_is(value, temp)));
        assert!(matches!(&args[1], CallArg::Value(value) if path_is(value, "collision")));
        assert_canonical_positions(&canonical, true);
    }

    #[test]
    fn normalization_is_idempotent_and_printable_by_construction() {
        let original = call_with_args(
            "apply",
            vec![
                let_("collision", call("effect_a"), path("collision"), 80),
                seq(call("effect_b"), path("collision")),
            ],
        );
        let once = canonical(original);
        let twice = canonical(once.clone());
        assert_eq!(twice, once);
        assert_canonical_positions(&twice, true);
    }
}

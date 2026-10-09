//! Conservative effect proofs over validated phase artifacts.
//!
//! Declaration purity permits calls through parameters. A pure declaration
//! therefore proves effect-free execution only when its supplied values cannot
//! carry host capabilities. Reading a path or constructing a closure requires
//! no such proof; evaluating the value and invoking it are separate questions.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{CallArg, Expr, ImportKind, Item, Module, PathSegment, Phase, Purity};
use crate::pass::resolve::Package;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PureFunctions(BTreeMap<String, BTreeSet<String>>);

impl PureFunctions {
    pub(crate) fn contains_head(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub(crate) fn remove_head(&mut self, name: &str) {
        self.0.remove(name);
    }

    pub(crate) fn for_module<P: Phase<FnPurity = Purity>>(
        package: &Package<P>,
        module: &Module<P>,
    ) -> Self {
        let mut result = Self::default();
        for item in &module.items {
            if let Item::FnDef(def) = item
                && def.purity.is_pure()
            {
                result
                    .0
                    .entry(def.name.clone())
                    .or_default()
                    .insert(String::new());
            }
        }
        for import in &module.imports {
            match &import.kind {
                ImportKind::Selective { items, from } => {
                    let Some(provider) = package.module(&from.segments.join("/")) else {
                        continue;
                    };
                    for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                        if provider
                            .scope
                            .lookup(name)
                            .and_then(|id| {
                                crate::pass::resolve::declaration_by_id(&provider.module, id)
                            })
                            .and_then(|declaration| declaration.fn_def())
                            .is_some_and(|def| def.purity.is_pure())
                        {
                            result
                                .0
                                .entry(name.to_owned())
                                .or_default()
                                .insert(String::new());
                        }
                    }
                }
                ImportKind::Qualified { path, alias } => {
                    let Some(provider) = package.module(&path.segments.join("/")) else {
                        continue;
                    };
                    for item in &provider.module.items {
                        if let Item::FnDef(def) = item
                            && def.purity.is_pure()
                        {
                            result
                                .0
                                .entry(alias.clone())
                                .or_default()
                                .insert(def.name.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        result
    }

    pub(crate) fn contains(&self, path: &[PathSegment]) -> bool {
        match path {
            [name] => self
                .0
                .get(name.as_str())
                .is_some_and(|members| members.contains("")),
            [head, member] => self
                .0
                .get(head.as_str())
                .is_some_and(|members| members.contains(member.as_str())),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Proof {
    Evaluation,
    CapabilityFree,
}

pub(crate) fn evaluation_is_pure<P: Phase>(
    expression: &Expr<P>,
    declared_pure: impl Fn(&[PathSegment]) -> bool,
) -> bool {
    analyze(
        expression,
        Proof::Evaluation,
        &declared_pure,
        &mut Vec::new(),
    )
}

fn analyze<'a, P: Phase>(
    expression: &'a Expr<P>,
    proof: Proof,
    declared_pure: &impl Fn(&[PathSegment]) -> bool,
    locals: &mut Vec<(&'a str, bool)>,
) -> bool {
    match expression {
        Expr::Path { segments, .. } => {
            proof == Proof::Evaluation
                || match segments.as_slice() {
                    [name] => locals
                        .iter()
                        .rev()
                        .find(|(bound, _)| *bound == name.as_str())
                        .map_or_else(|| declared_pure(segments), |(_, safe)| *safe),
                    _ => declared_pure(segments),
                }
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => true,
        Expr::FnExpr { sig, body, .. } => {
            if proof == Proof::Evaluation {
                return true;
            }
            let saved = locals.len();
            locals.extend(sig.value_param_names().map(|name| (name, true)));
            let result = analyze(body, Proof::CapabilityFree, declared_pure, locals);
            locals.truncate(saved);
            result
        }
        Expr::Call { callee, args, .. } => {
            analyze(callee, Proof::CapabilityFree, declared_pure, locals)
                && args.iter().all(|arg| match arg {
                    CallArg::Type(_) => true,
                    CallArg::Value(value) => {
                        analyze(value, Proof::CapabilityFree, declared_pure, locals)
                    }
                })
        }
        Expr::Let {
            name, value, body, ..
        } => {
            if !analyze(value, proof, declared_pure, locals) {
                return false;
            }
            locals.push((name, proof == Proof::CapabilityFree));
            let result = analyze(body, proof, declared_pure, locals);
            locals.pop();
            result
        }
        Expr::Seq { value, body, .. } => {
            analyze(value, proof, declared_pure, locals)
                && analyze(body, proof, declared_pure, locals)
        }
        Expr::EnrichedTuple { items, .. } => items
            .iter()
            .all(|value| analyze(value, proof, declared_pure, locals)),
        Expr::EnrichedRecord { fields, .. } => fields
            .iter()
            .all(|field| analyze(&field.value, proof, declared_pure, locals)),
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            analyze(target, proof, declared_pure, locals)
        }
        Expr::EnrichedInject { payload, .. } => analyze(payload, proof, declared_pure, locals),
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            if !analyze(scrutinee, proof, declared_pure, locals) {
                return false;
            }
            arms.iter().all(|arm| {
                locals.push((&arm.param, proof == Proof::CapabilityFree));
                let result = analyze(&arm.body, proof, declared_pure, locals);
                locals.pop();
                result
            })
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            analyze(cond, proof, declared_pure, locals)
                && analyze(then_branch, proof, declared_pure, locals)
                && analyze(else_branch, proof, declared_pure, locals)
        }
        _ => false,
    }
}

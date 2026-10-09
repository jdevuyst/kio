//! Rendering of residual values for diagnostics and REPL expression queries.

use std::collections::BTreeMap;
#[cfg(any(feature = "repl-core", test))]
use std::collections::BTreeSet;

use super::{
    DataConstructorIdentity, EvalAstExpr, EvalFunction, EvalFunctionOrigin, NewtypeCtorIdentity,
    StructuralRecurDataIdentity, Value, collect_value_param_names,
};
#[cfg(any(feature = "repl-core", test))]
use super::{module_import_path, module_path_string};
use crate::ast::{CallArg, Expr};
#[cfg(any(feature = "repl-core", test))]
use crate::ast::{ImportKind, Module};
use crate::comptime::ComptimeBuiltin;

// =========================================================================
// Pretty-printing for diagnostics
// =========================================================================

/// Render a [`Value`] as a human-readable string for diagnostic
/// output. Pure ground values (`Unit`, `Bool`, literal atoms, string
/// atoms) print as their source form; `Stuck` applications render as
/// `callee(arg, …)`; closures print as source-shaped Kio' expressions.
pub fn value_to_string(v: &Value) -> String {
    ValueRenderer {
        newtype_type_heads: None,
        render_closure_origins: false,
    }
    .render(v)
}

/// Render a residual value in the scope of `module` for the REPL.
///
/// Evaluator values preserve the declaration identity of newtype
/// constructors and projectors. A residual can originate in an inlined
/// function from another module, where its source-local import alias has no
/// meaning in the REPL's current module. Re-spell those exact identities
/// through the current scope and introduce ordinary qualified imports only
/// when the scope has no spelling for their owner module.
#[cfg(any(feature = "repl-core", test))]
pub(crate) fn value_to_repl_string<P>(v: &Value, module: &Module<P>) -> String
where
    P: crate::pass::resolve::ResolvePhase,
{
    let mut scan = ReplReferenceScan::default();
    scan_repl_value(v, &[], &mut scan);

    let current_module = module_path_string(module);
    let mut requalifier = crate::pass::typecheck_core::PrimeTypeRequalifier::new(module);
    let mut newtype_type_heads = BTreeMap::new();
    for (owner, type_name) in &scan.referenced_newtypes {
        let forbidden = scan.forbidden_for_owner(owner);
        let can_use_unqualified = !forbidden.contains(type_name)
            && (owner == &current_module
                || module.imports.iter().any(|import_| {
                    matches!(
                        &import_.kind,
                        ImportKind::Selective { items, from }
                            if module_import_path(from) == *owner
                                && items
                                    .iter()
                                    .filter_map(crate::ast::ImportItem::as_name)
                                    .any(|name| name == *type_name)
                    )
                }));
        let head = if can_use_unqualified {
            type_name.clone()
        } else {
            format!(
                "{}.{}",
                requalifier.ensure_module_import_avoiding(owner, &forbidden),
                type_name
            )
        };
        newtype_type_heads.insert((owner.clone(), type_name.clone()), head);
    }

    let expression = ValueRenderer {
        newtype_type_heads: Some(&newtype_type_heads),
        render_closure_origins: true,
    }
    .render(v);
    let imports = requalifier.take_imports();
    if imports.is_empty() {
        expression
    } else {
        let prelude = imports
            .iter()
            .map(crate::pretty::pretty_import)
            .collect::<Vec<_>>()
            .join("\n");
        format!("{prelude}\n\n{expression}")
    }
}

#[cfg(any(feature = "repl-core", test))]
#[derive(Default)]
struct ReplReferenceScan {
    referenced_newtypes: BTreeSet<(String, String)>,
    forbidden_by_owner: BTreeMap<String, BTreeSet<String>>,
    atom_heads: BTreeSet<String>,
}

#[cfg(any(feature = "repl-core", test))]
impl ReplReferenceScan {
    fn record_newtype<'a>(
        &mut self,
        identity: &NewtypeCtorIdentity,
        bound: impl Iterator<Item = &'a String>,
    ) {
        self.referenced_newtypes
            .insert((identity.module_path.clone(), identity.type_name.clone()));
        self.forbidden_by_owner
            .entry(identity.module_path.clone())
            .or_default()
            .extend(bound.cloned());
    }

    fn record_atom(&mut self, name: &str) {
        if let Some(head) = name.split('.').next()
            && !head.is_empty()
        {
            self.atom_heads.insert(head.to_owned());
        }
    }

    fn forbidden_for_owner(&self, owner: &str) -> BTreeSet<String> {
        let mut forbidden = self.atom_heads.clone();
        if let Some(scoped) = self.forbidden_by_owner.get(owner) {
            forbidden.extend(scoped.iter().cloned());
        }
        forbidden
    }
}

#[cfg(any(feature = "repl-core", test))]
fn scan_repl_value(v: &Value, enclosing_bound: &[String], scan: &mut ReplReferenceScan) {
    match v {
        Value::NewtypeMember(member) => {
            scan.record_newtype(&member.constructor, enclosing_bound.iter());
        }
        Value::Data(data) => {
            if data.structural_recur_identity().is_some() {
                return;
            }
            if let DataConstructorIdentity::Newtype(identity) = &data.identity {
                scan.record_newtype(identity, enclosing_bound.iter());
            }
            for arg in data.args() {
                scan_repl_value(arg, enclosing_bound, scan);
            }
        }
        Value::CaseSplit {
            scrutinee,
            left_payload,
            left,
            right_payload,
            right,
        } => {
            scan_repl_value(scrutinee, enclosing_bound, scan);
            let mut left_bound = enclosing_bound.to_vec();
            left_bound.push(left_payload.clone());
            scan_repl_value(left, &left_bound, scan);
            let mut right_bound = enclosing_bound.to_vec();
            right_bound.push(right_payload.clone());
            scan_repl_value(right, &right_bound, scan);
        }
        Value::Seq { value, body } => {
            scan_repl_value(value, enclosing_bound, scan);
            scan_repl_value(body, enclosing_bound, scan);
        }
        Value::Stuck(callee, args) => {
            scan_repl_value(callee, enclosing_bound, scan);
            for arg in args.iter() {
                scan_repl_value(arg, enclosing_bound, scan);
            }
        }
        Value::Atom(name) => scan.record_atom(name),
        #[cfg(feature = "surface")]
        Value::Projected(_) => {}
        Value::Unit
        | Value::Bool(_)
        | Value::IntAtom(_, _)
        | Value::FloatAtom(_, _)
        | Value::StrAtom(_)
        | Value::ReflType(_)
        | Value::CheckedTerm(_)
        | Value::ReflTypeArity(_)
        | Value::ReflTypeVar { .. }
        | Value::ReflTypeName { .. }
        | Value::DiagnosticText(_)
        | Value::Primitive(_)
        | Value::EvalClosure { .. }
        | Value::StructuralRecur { .. } => {}
    }
}

struct ValueRenderer<'a> {
    newtype_type_heads: Option<&'a BTreeMap<(String, String), String>>,
    render_closure_origins: bool,
}

impl ValueRenderer<'_> {
    fn render(&self, v: &Value) -> String {
        match v {
            Value::Unit => "()".to_string(),
            Value::Bool(true) => ".t".to_string(),
            Value::Bool(false) => ".f".to_string(),
            Value::IntAtom(d, ann) | Value::FloatAtom(d, ann) => match ann {
                Some(t) => format!("{d}({})", t.rsplit('.').next().unwrap_or(t)),
                None => d.clone(),
            },
            Value::StrAtom(s) => format!("{s:?}"),
            Value::DiagnosticText(s) => format!("{s:?}"),
            Value::ReflType(t) => format!(
                "<type {}>",
                crate::pass::typecheck_core::display_type(t.as_type())
            ),
            Value::CheckedTerm(_) => "<checked_term>".to_string(),
            #[cfg(feature = "surface")]
            Value::Projected(projected) => if projected.is_type() {
                "<projected-type>"
            } else {
                "<projected-checked-term>"
            }
            .to_string(),
            Value::ReflTypeArity(n) => format!("<arity {n}>"),
            Value::ReflTypeVar { name, arity } => format!("<type-var {name}/{arity}>"),
            Value::ReflTypeName {
                segments,
                param_arities,
            } => format!("<type-name {} {:?}>", segments.join("."), param_arities),
            Value::StructuralRecur { .. } => "<comptime-recur>".to_string(),
            Value::Primitive(builtin) => builtin.public_name().to_string(),
            Value::NewtypeMember(member) => self.newtype_head(&member.constructor).map_or_else(
                || member.display.clone(),
                |head| format!("{head}.{}", member.member_name),
            ),
            Value::Atom(name) => name.clone(),
            Value::Data(data) => {
                if let Some(identity) = data.structural_recur_identity() {
                    return match identity {
                        StructuralRecurDataIdentity::Primitive { .. } => {
                            ComptimeBuiltin::StructuralRecur.public_name().to_owned()
                        }
                        StructuralRecurDataIdentity::InitialRejected => {
                            "<structural-recur-initial-rejected>".to_owned()
                        }
                    };
                }
                let ctor = match &data.identity {
                    DataConstructorIdentity::Newtype(identity) => {
                        self.newtype_head(identity).map_or_else(
                            || data.ctor().to_owned(),
                            |head| format!("{head}.{}", identity.constructor_name),
                        )
                    }
                    DataConstructorIdentity::Core(_) => data.ctor().to_owned(),
                    DataConstructorIdentity::StructuralRecur(_) => {
                        unreachable!("structural-recursion evaluator data renders opaquely")
                    }
                    #[cfg(feature = "surface")]
                    DataConstructorIdentity::MarkedComptimeProof(_) => {
                        "<marked-comptime-proof>".to_owned()
                    }
                    #[cfg(feature = "surface")]
                    DataConstructorIdentity::FillContext(_) => "<fill-context>".to_owned(),
                };
                let arg_strs: Vec<String> =
                    data.args().iter().map(|arg| self.render(arg)).collect();
                format!("{ctor}({})", arg_strs.join(", "))
            }
            Value::CaseSplit {
                scrutinee,
                left_payload,
                left,
                right_payload,
                right,
            } => format!(
                "case {} {{ left({left_payload}) -> {}, right({right_payload}) -> {} }}",
                self.render(scrutinee),
                self.render(left),
                self.render(right),
            ),
            Value::Seq { value, body } => {
                format!("{}; {}", self.render(value), self.render(body))
            }
            Value::Stuck(c, args) => {
                let arg_strs: Vec<String> = args.iter().map(|arg| self.render(arg)).collect();
                format!("{}({})", self.render(c), arg_strs.join(", "))
            }
            Value::EvalClosure { function, .. } => {
                if self.render_closure_origins {
                    render_closure_origin(function)
                } else {
                    render_closure_source(function)
                }
            }
        }
    }

    fn newtype_head(&self, identity: &NewtypeCtorIdentity) -> Option<&str> {
        self.newtype_type_heads?
            .get(&(identity.module_path.clone(), identity.type_name.clone()))
            .map(String::as_str)
    }
}

fn render_closure_source(function: &EvalFunction) -> String {
    let params = function
        .params
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        ".({params}) {{ {} }}",
        render_eval_expr(&function.diagnostic_body)
    )
}

fn render_eval_expr(expr: &EvalAstExpr) -> String {
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Unit { .. } => "()".to_string(),
        Expr::BoolLit { value: true, .. } => ".t".to_string(),
        Expr::BoolLit { value: false, .. } => ".f".to_string(),
        Expr::IntLit { digits, .. } | Expr::FloatLit { digits, .. } => digits.clone(),
        Expr::StrLit { value, .. } => format!("{value:?}"),
        Expr::Path { segments, .. } => segments.join("."),
        Expr::Call { callee, args, .. } => {
            let args = args
                .iter()
                .filter_map(|arg| match arg {
                    CallArg::Value(value) => Some(render_eval_expr(value)),
                    CallArg::Type(_) => None,
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({args})", render_eval_expr(callee))
        }
        Expr::Let {
            name, value, body, ..
        } => format!(
            "let {name} = {}; {}",
            render_eval_expr(value),
            render_eval_expr(body)
        ),
        Expr::Seq { value, body, .. } => {
            format!("{}; {}", render_eval_expr(value), render_eval_expr(body))
        }
        Expr::FnExpr { sig, body, .. } => {
            let params = collect_value_param_names(&sig.params).join(", ");
            format!(".({params}) {{ {} }}", render_eval_expr(body))
        }
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Tuple { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::OpChain { ext, .. }
        | Expr::RecCall { ext, .. }
        | Expr::Ufcs { ext, .. }
        | Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
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

fn render_closure_origin(function: &EvalFunction) -> String {
    let owner = if function.owner_module_path.is_empty() {
        String::new()
    } else {
        format!("{}.", function.owner_module_path)
    };
    let identity = match &function.source_origin {
        EvalFunctionOrigin::Named { item } => format!("{owner}{item}"),
        EvalFunctionOrigin::Anonymous { enclosing, span } => {
            let enclosing = enclosing
                .as_deref()
                .map_or_else(String::new, |name| format!("{name}::"));
            format!("{owner}{enclosing}<fn@{}-{}>", span.start, span.end)
        }
    };
    let params = function
        .value_groups
        .iter()
        .map(|group| {
            let names = function.params[group.param_start..group.param_start + group.param_count]
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("({names})")
        })
        .collect::<String>();
    format!("<closure {identity}{params}>")
}

//! Source-local ownership of numbered placeholder references.

use std::collections::HashSet;

#[cfg(feature = "surface")]
use crate::ast::PlaceholderState;
use crate::ast::{
    CallArg, ElaboratorCall, Expr, OpChainKind, ParamPattern, ParamPatternElem, PathSegment,
    SignatureParam,
};
use crate::error::Error;
use crate::span::Span;

pub(crate) struct References<'a> {
    pub slot_count: usize,
    pub occurrences: Vec<(&'a mut PathSegment, u32)>,
}

/// Validation collects mutable locations without changing them. A failed owner
/// therefore remains source-shaped, including when a later index is invalid.
pub(crate) fn classify<'a>(
    stem: &str,
    body: &'a mut Expr,
    owner: Span,
) -> Result<References<'a>, Error> {
    let mut walk = Classifier {
        stem,
        bound: HashSet::new(),
        references: References {
            slot_count: 0,
            occurrences: Vec::new(),
        },
    };
    walk.expr(body)?;
    if walk.references.slot_count == 0 {
        return Err(Error::parse(owner, format!(
            "placeholder lambda `.{stem}.` must contain at least one unshadowed `{stem}N` reference"
        )).with_help("add a numbered reference, or write an explicit `.() { ... }` lambda"));
    }
    Ok(walk.references)
}

/// Operator expansion can introduce ordinary paths with the same spelling.
/// Consume the source state before folding, and reuse the classified state on
/// repeated folds or the direct-desugar path.
#[cfg(feature = "surface")]
pub(crate) fn prepare(
    stem: &PathSegment,
    state: &mut PlaceholderState,
    body: &mut Expr,
    owner: Span,
) -> Result<usize, Error> {
    if let PlaceholderState::Classified { slot_count } = state {
        return Ok(*slot_count);
    }
    let references = classify(&stem.name, body, owner)?;
    let slot_count = references.slot_count;
    for (reference, slot) in references.occurrences {
        reference.name = local_name(slot);
    }
    *state = PlaceholderState::Classified { slot_count };
    Ok(slot_count)
}

#[cfg(feature = "surface")]
pub(crate) fn local_name(slot: u32) -> String {
    format!("__p{slot}__")
}

struct Classifier<'s, 'a> {
    stem: &'s str,
    bound: HashSet<String>,
    references: References<'a>,
}

impl<'a> Classifier<'_, 'a> {
    fn path(&mut self, segments: &'a mut [PathSegment]) -> Result<(), Error> {
        let [reference] = segments else { return Ok(()) };
        if self.bound.contains(&reference.name) {
            return Ok(());
        }
        let Some(digits) = reference.name.strip_prefix(self.stem) else {
            return Ok(());
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(());
        }
        if digits.starts_with('0') {
            return Err(Error::parse(
                reference.span,
                "placeholder indices are positive decimal integers without leading zeroes",
            ));
        }
        let slot = digits.parse::<u32>().map_err(|_| {
            Error::parse(
                reference.span,
                format!(
                    "placeholder index `{}` is out of range for u32",
                    reference.name
                ),
            )
        })?;
        self.references.slot_count = self.references.slot_count.max(slot as usize);
        self.references.occurrences.push((reference, slot));
        Ok(())
    }

    fn bind(&mut self, name: &str, introduced: &mut Vec<String>) {
        if name != "_" && self.bound.insert(name.to_owned()) {
            introduced.push(name.to_owned());
        }
    }

    fn pattern(&mut self, pattern: &ParamPattern, introduced: &mut Vec<String>) {
        for elem in &pattern.elems {
            match elem {
                ParamPatternElem::Bind { name, .. } => self.bind(name, introduced),
                ParamPatternElem::Tuple(inner) => self.pattern(inner, introduced),
                ParamPatternElem::BindTuple { name, inner, .. } => {
                    self.bind(name, introduced);
                    self.pattern(inner, introduced);
                }
            }
        }
    }

    fn binding(
        &mut self,
        name: &str,
        pattern: Option<&ParamPattern>,
        introduced: &mut Vec<String>,
    ) {
        self.bind(name, introduced);
        if let Some(pattern) = pattern {
            self.pattern(pattern, introduced);
        }
    }

    fn leave(&mut self, introduced: Vec<String>) {
        for name in introduced {
            self.bound.remove(&name);
        }
    }

    fn args(&mut self, args: &'a mut [CallArg]) -> Result<(), Error> {
        for arg in args {
            if let CallArg::Value(value) = arg {
                self.expr(value)?;
            }
        }
        Ok(())
    }

    fn expr(&mut self, expr: &'a mut Expr) -> Result<(), Error> {
        match expr {
            Expr::BlockCall { prefix, blocks, .. } => {
                for value in prefix {
                    self.expr(value)?;
                }
                for block in blocks {
                    let mut introduced = Vec::new();
                    for item in &mut block.items {
                        match item {
                            crate::ast::NeutralItem::Expression { value, .. } => {
                                self.expr(value)?
                            }
                            crate::ast::NeutralItem::Binding {
                                name,
                                pattern,
                                value,
                                ..
                            }
                            | crate::ast::NeutralItem::ExistentialBinding {
                                name,
                                pattern,
                                value,
                                ..
                            } => {
                                self.expr(value)?;
                                self.binding(name, pattern.as_ref(), &mut introduced);
                            }
                            crate::ast::NeutralItem::RowBinding { entries, value, .. } => {
                                self.expr(value)?;
                                for entry in entries {
                                    self.bind(&entry.local, &mut introduced);
                                }
                            }
                        }
                    }
                    self.leave(introduced);
                }
            }
            Expr::Path { segments, .. } => self.path(segments)?,
            Expr::Call { callee, args, .. } => {
                self.expr(callee)?;
                self.args(args)?;
            }
            Expr::RecCall { args, .. } | Expr::UserElaborator { args, .. } => self.args(args)?,
            Expr::FnExpr { sig, body, .. } => {
                let mut introduced = Vec::new();
                for param in &sig.params {
                    if let SignatureParam::Value(param) = param {
                        self.binding(&param.name, param.pattern.as_ref(), &mut introduced);
                    }
                }
                self.expr(body)?;
                self.leave(introduced);
            }
            Expr::Let {
                name,
                pattern,
                value,
                body,
                ..
            } => {
                self.expr(value)?;
                let mut introduced = Vec::new();
                self.binding(name, pattern.as_ref(), &mut introduced);
                self.expr(body)?;
                self.leave(introduced);
            }
            Expr::RowLet {
                entries,
                value,
                body,
                ..
            } => {
                self.expr(value)?;
                let mut introduced = Vec::new();
                for entry in entries {
                    self.bind(&entry.local, &mut introduced);
                }
                self.expr(body)?;
                self.leave(introduced);
            }
            Expr::Seq { value, body, .. } => {
                self.expr(value)?;
                self.expr(body)?;
            }
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item)?;
                }
            }
            Expr::FnPlaceholder { .. } => {}
            Expr::LabelValue { labels, .. } => {
                for label in labels {
                    self.expr(&mut label.value)?;
                }
            }
            Expr::Elaborator { call, .. } => match call {
                ElaboratorCall::FieldAccess { receiver, .. } => self.expr(receiver)?,
                ElaboratorCall::FieldUpdate { receiver, updates } => {
                    self.expr(receiver)?;
                    for update in updates {
                        self.expr(&mut update.value)?;
                    }
                }
            },
            Expr::Ufcs {
                receiver,
                callee_segments,
                args,
                bang,
                ..
            } => {
                self.expr(receiver)?;
                if bang.is_none() {
                    self.path(callee_segments)?;
                }
                self.args(args)?;
            }
            Expr::OpChain { kind, .. } => match kind {
                OpChainKind::Normal {
                    slots: children, ..
                }
                | OpChainKind::Variadic {
                    elements: children, ..
                } => {
                    for child in children {
                        self.expr(child)?;
                    }
                }
            },
            Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => {}
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
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
        Ok(())
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::{Item, Module};
    use crate::pass::{desugar, op_fold, parser};

    fn function_mut(module: &mut Module) -> &mut Expr {
        module
            .items
            .iter_mut()
            .rev()
            .find_map(|item| match item {
                Item::FnDef(def) => Some(&mut def.body),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn classified_state_survives_repeat_fold_and_serialization() {
        let source = "module m; fn x2(a: ., b: .) -> . { a } \
            op _ + _ { impl x2; }; fn run() { .x. { x1 + () } }";
        let once =
            op_fold::fold_package(vec![("m.kio".into(), parser::parse(source).unwrap())]).unwrap();
        let bytes = postcard::to_allocvec(&once).unwrap();
        let restored = postcard::from_bytes(&bytes).unwrap();
        let mut twice = op_fold::fold_package(restored).unwrap();
        assert_eq!(once, twice);
        let Expr::FnPlaceholder { state, body, .. } = function_mut(&mut twice[0].1) else {
            panic!("placeholder")
        };
        assert_eq!(*state, PlaceholderState::Classified { slot_count: 1 });
        let Expr::Call { callee, .. } = body.as_ref() else {
            panic!("folded operator")
        };
        assert!(matches!(callee.as_ref(), Expr::Path { segments, .. } if segments[0].name == "x2"));
    }

    #[test]
    fn failed_classification_keeps_source_tree_unchanged() {
        let mut module = parser::parse("module m; fn run() { .x. { pair(x1, x2) } }").unwrap();
        let Expr::FnPlaceholder { body, .. } = function_mut(&mut module) else {
            panic!("placeholder")
        };
        let Expr::Call { args, .. } = body.as_mut() else {
            panic!("call")
        };
        let CallArg::Value(Expr::Path { segments, .. }) = &mut args[1] else {
            panic!("path")
        };
        segments[0].name = "x01".into();
        let original = module.clone();
        for _ in 0..2 {
            let Expr::FnPlaceholder {
                stem,
                state,
                body,
                meta,
                ..
            } = function_mut(&mut module)
            else {
                panic!("placeholder")
            };
            assert!(prepare(stem, state, body, meta.span).is_err());
            assert_eq!(module, original);
        }
    }

    #[test]
    fn direct_desugar_matches_the_folded_route() {
        let source = "module m; fn run() { .x. { pair(x1, .y. { pair(y1, p1) }) } }";
        let parsed = parser::parse(source).unwrap();
        let direct = desugar::desugar_module(parsed.clone()).unwrap();
        let (_, folded) = op_fold::fold_package(vec![("m.kio".into(), parsed)])
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(direct, desugar::desugar_module(folded).unwrap());
        let Item::FnDef(def) = &direct.items[0] else {
            panic!("function")
        };
        let Expr::FnExpr { sig, .. } = &def.body else {
            panic!("lowered lambda")
        };
        let SignatureParam::Value(param) = &sig.params[0] else {
            panic!("value parameter")
        };
        assert_ne!(
            param.name, "p1",
            "outer lambda must not capture a free name in the inner owner"
        );
    }

    #[test]
    fn ufcs_placeholder_callee_uses_the_generated_parameter() {
        let module = desugar::desugar_module(
            parser::parse("module m; fn run() { .x. { p1.>x1 } }").unwrap(),
        )
        .unwrap();
        let Item::FnDef(def) = &module.items[0] else {
            panic!("function")
        };
        let Expr::FnExpr { sig, body, .. } = &def.body else {
            panic!("lowered lambda")
        };
        let SignatureParam::Value(param) = &sig.params[0] else {
            panic!("value parameter")
        };
        let Expr::Ufcs {
            receiver,
            callee_segments,
            ..
        } = body.as_ref()
        else {
            panic!("ufcs")
        };
        assert_ne!(param.name, "p1");
        assert_eq!(callee_segments[0].name, param.name);
        assert!(
            matches!(receiver.as_ref(), Expr::Path { segments, .. } if segments[0].name == "p1")
        );
    }
}

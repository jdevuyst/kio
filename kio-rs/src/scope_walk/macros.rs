//! Source-local macro selection before source binding structure is lowered.

use std::collections::HashMap;

use crate::ast::*;
use crate::span::Span;

#[derive(Debug, Default)]
pub struct MacroCalls {
    heads: HashMap<Span, String>,
    pub(crate) direct: HashMap<Span, String>,
    pub(crate) max_node_id: u64,
}

impl MacroCalls {
    pub fn contains(&self, span: Span) -> bool {
        self.heads.contains_key(&span)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Span, &String)> {
        self.heads.iter()
    }
}

/// Exact macro heads from tagged imports, earlier declarations and lexical scope.
/// Provider contents and inferred types do not participate in this classification.
pub fn macro_calls(module: &Module<Surface>) -> MacroCalls {
    macro_calls_for_items(&module.imports, &module.items)
}

/// Classify an inspector expression in the module's retained end scope.
pub fn macro_calls_for_expression(module: &Module, expression: &Expr) -> MacroCalls {
    let mut walk = Classifier {
        scope: super::MacroScope::at_module_end(module),
        facts: MacroCalls::default(),
    };
    walk.expr(expression);
    walk.facts
}

pub fn macro_calls_for_items(imports: &[Import], items: &[Item]) -> MacroCalls {
    let mut walk = Classifier {
        scope: super::MacroScope::from_items(imports, items),
        facts: MacroCalls::default(),
    };
    for item in items {
        match item {
            Item::FnDef(def) => {
                walk.function(def);
            }
            Item::RecGroup(group, _) => {
                for def in &group.members {
                    walk.function(def);
                }
            }
            Item::Equiv(equiv, _) => {
                for term in &equiv.terms {
                    let mark = walk.mark();
                    walk.signature(&equiv.sig);
                    walk.expr(&term.body);
                    walk.restore(mark);
                }
            }
            _ => {}
        }
    }
    walk.facts
}

#[derive(Default)]
struct Classifier {
    scope: super::MacroScope,
    facts: MacroCalls,
}

impl Classifier {
    fn mark(&self) -> super::MacroScopeMark {
        self.scope.mark()
    }
    fn restore(&mut self, mark: super::MacroScopeMark) {
        self.scope.restore(mark);
    }
    fn head(&mut self, head: &PathSegment) -> bool {
        if self.scope.is_macro(&head.name, head.span.start) {
            self.facts.heads.insert(head.span, head.name.clone());
            true
        } else {
            false
        }
    }

    fn id(&mut self, id: NodeId) {
        self.facts.max_node_id = self.facts.max_node_id.max(id.0);
    }

    fn pattern_ids(&mut self, pattern: &ParamPattern) {
        self.id(pattern.match_id);
        for elem in &pattern.elems {
            if let ParamPatternElem::Tuple(inner) | ParamPatternElem::BindTuple { inner, .. } = elem
            {
                self.pattern_ids(inner);
            }
        }
    }

    fn binding(&mut self, name: &str, pattern: Option<&ParamPattern>) {
        if let Some(pattern) = pattern {
            self.pattern_ids(pattern);
        }
        self.scope.binding(name, pattern);
    }

    fn signature(&mut self, signature: &Signature<Surface>) {
        for param in &signature.params {
            if let SignatureParam::Value(param) = param {
                self.binding(&param.name, param.pattern.as_ref());
            }
        }
    }

    fn function<Purity>(&mut self, def: &FnDef<Surface, Purity>) {
        let mark = self.mark();
        self.signature(&def.sig);
        self.expr(&def.body);
        self.restore(mark);
    }

    fn args(&mut self, args: &[CallArg]) {
        for arg in args {
            if let CallArg::Value(value) = arg {
                self.expr(value);
            }
        }
    }

    fn block(&mut self, block: &NeutralBlock) {
        let mark = self.mark();
        for item in &block.items {
            self.expr(item.value());
            match item {
                NeutralItem::Binding { name, pattern, .. }
                | NeutralItem::ExistentialBinding { name, pattern, .. } => {
                    self.binding(name, pattern.as_ref())
                }
                NeutralItem::RowBinding { entries, .. } => {
                    for entry in entries {
                        self.id(entry.access_ext);
                        self.scope.bind(&entry.local);
                    }
                }
                NeutralItem::Expression { .. } => {}
            }
        }
        self.restore(mark);
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::BlockCall {
                id,
                head,
                prefix,
                blocks,
                ..
            } => {
                self.id(*id);
                self.head(head);
                for value in prefix {
                    self.expr(value);
                }
                for block in blocks {
                    self.block(block);
                }
            }
            Expr::Call {
                callee, args, meta, ..
            } => {
                if let Expr::Path { segments, .. } = callee.as_ref()
                    && let [head] = segments.as_slice()
                    && self.head(head)
                {
                    self.facts.direct.insert(meta.span, head.name.clone());
                }
                self.expr(callee);
                self.args(args);
            }
            Expr::Ufcs {
                receiver,
                callee_segments,
                args,
                ext,
                ..
            } => {
                self.id(*ext);
                if let [head] = callee_segments.as_slice() {
                    self.head(head);
                }
                self.expr(receiver);
                self.args(args);
            }
            Expr::UserMacro { args, ext, .. } => {
                self.id(*ext);
                self.args(args);
            }
            Expr::RecCall { args, .. } => self.args(args),
            Expr::FnExpr { sig, body, .. } => {
                let mark = self.mark();
                self.signature(sig);
                self.expr(body);
                self.restore(mark);
            }
            Expr::FnPlaceholder { stem, body, .. } => {
                let mark = self.mark();
                self.scope.placeholder(&stem.name);
                self.expr(body);
                self.restore(mark);
            }
            Expr::Let {
                name,
                pattern,
                value,
                body,
                ..
            } => {
                self.expr(value);
                let mark = self.mark();
                self.binding(name, pattern.as_ref());
                self.expr(body);
                self.restore(mark);
            }
            Expr::RowLet {
                entries,
                value,
                body,
                ..
            } => {
                self.expr(value);
                let mark = self.mark();
                for entry in entries {
                    self.id(entry.access_ext);
                    self.scope.bind(&entry.local);
                }
                self.expr(body);
                self.restore(mark);
            }
            Expr::Seq { value, body, .. } => {
                self.expr(value);
                self.expr(body);
            }
            Expr::Tuple { items, .. } => {
                for item in items {
                    self.expr(item);
                }
            }
            Expr::LabelValue { labels, ext, .. } => {
                self.id(*ext);
                for label in labels {
                    self.expr(&label.value);
                }
            }
            Expr::Elaborator { call, ext, .. } => {
                self.id(*ext);
                match call {
                    ElaboratorCall::FieldAccess { receiver, .. } => self.expr(receiver),
                    ElaboratorCall::FieldUpdate { base, updates } => {
                        if let Some(receiver) = base.receiver() {
                            self.expr(receiver);
                        }
                        for update in updates {
                            self.expr(&update.value);
                        }
                    }
                }
            }
            Expr::OpChain { kind, .. } => {
                let (OpChainKind::Normal {
                    slots: children, ..
                }
                | OpChainKind::Variadic {
                    elements: children, ..
                }) = kind;
                for child in children {
                    self.expr(child);
                }
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::StrLit { .. }
            | Expr::BoolLit { .. } => {}
            Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(source: &str) -> Vec<(usize, String)> {
        let module = crate::pass::parser::parse(source).unwrap();
        let mut heads: Vec<_> = macro_calls(&module)
            .iter()
            .map(|(span, name)| {
                assert_eq!(&source[span.start as usize..span.end as usize], name);
                (span.start as usize, name.clone())
            })
            .collect();
        heads.sort();
        heads
    }

    #[test]
    fn local_bindings_win_and_initializers_keep_the_previous_scope() {
        let source = "module main; import provider(macro weave as selected);\n\
            fn first() { selected(()) }\n\
            fn parameter(selected: . -> .) { selected(()) }\n\
            fn binding() { let selected = selected(()); selected(()) }\n\
            fn nested() { (.(selected: . -> .) { selected(()) }, selected(())) }";
        let expected = ["selected(()) }", "selected(());", "selected(())) }"]
            .map(|needle| (source.find(needle).unwrap(), "selected".to_owned()));
        assert_eq!(selected(source), expected);
    }

    #[test]
    fn grouping_and_ufcs_keep_direct_local_identity() {
        let source = "module main; import provider(macro weave as selected);\n\
            fn run(value: .) { let _ = (selected)(value); let _ = ((selected))(value);\n\
                let _ = value.>selected; let _ = selected.<value; selected(value)(value) }";
        let heads = selected(source);
        assert_eq!(heads.len(), 5);
        assert!(heads.iter().all(|(_, name)| name == "selected"));
    }

    #[test]
    fn declaration_order_and_numbered_ownership_are_local() {
        let source = "module main; import provider(macro x1, macro x2);\n\
            fn before() { local(()) }\n\
            macro local: . -> . { impl helper; }\n\
            fn after() { local(()) }\n\
            fn numbered() { .x. { (x1(()), .y. { (y1, x1(())) }) } }\n\
            fn binding() { let .x. = (); (x1(()), x2(())) }";
        let heads = selected(source);
        assert_eq!(
            heads
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["local", "x1"]
        );
    }

    #[test]
    fn same_spelled_ordinary_imports_never_acquire_macro_kind() {
        let source = "module main; import provider(weave); fn run() { weave(()) }";
        assert!(selected(source).is_empty());
    }
}

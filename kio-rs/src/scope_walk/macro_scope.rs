use std::collections::HashMap;

use crate::ast::{Import, ImportItem, ImportKind, Item, Module, ParamPattern, ParamPatternElem};

/// Source-only declaration availability and lexical value binders.
#[derive(Default)]
pub(crate) struct MacroScope {
    declarations: HashMap<String, u32>,
    locals: HashMap<String, usize>,
    introduced: Vec<String>,
    numbered: Vec<(String, usize)>,
    family: usize,
    next_family: usize,
}

pub(crate) type MacroScopeMark = (usize, usize, usize);

impl MacroScope {
    pub(crate) fn from_imports(imports: &[Import]) -> Self {
        let mut scope = Self::default();
        for import in imports {
            if let ImportKind::Selective { items, .. } = &import.kind {
                for item in items {
                    if matches!(
                        item,
                        ImportItem::Name {
                            macro_span: Some(_),
                            ..
                        }
                    ) {
                        scope.declare(item.as_local_name().unwrap(), import.span.end);
                    }
                }
            }
        }
        scope
    }

    pub(crate) fn from_items(imports: &[Import], items: &[Item]) -> Self {
        let mut scope = Self::from_imports(imports);
        for item in items {
            if let Item::Macro(def, _) = item {
                scope.declare(&def.name, item.span().end);
            }
        }
        scope
    }

    pub(crate) fn at_module_end(module: &Module) -> Self {
        let mut scope = Self::from_items(&module.imports, &module.items);
        for offset in scope.declarations.values_mut() {
            *offset = 0;
        }
        scope
    }

    pub(crate) fn declare(&mut self, name: &str, available: u32) {
        self.declarations
            .entry(name.to_owned())
            .and_modify(|old| *old = (*old).min(available))
            .or_insert(available);
    }

    pub(crate) fn is_macro(&self, name: &str, offset: u32) -> bool {
        if self.locals.contains_key(name) {
            return false;
        }
        if self.numbered.iter().rev().any(|(stem, family)| {
            *family == self.family
                && name.strip_prefix(stem).is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
        }) {
            return false;
        }
        self.declarations
            .get(name)
            .is_some_and(|available| *available <= offset)
    }

    pub(crate) fn mark(&self) -> MacroScopeMark {
        (self.introduced.len(), self.numbered.len(), self.family)
    }

    pub(crate) fn restore(&mut self, (names, numbered, family): MacroScopeMark) {
        for name in self.introduced.drain(names..).rev() {
            let count = self.locals.get_mut(&name).unwrap();
            *count -= 1;
            if *count == 0 {
                self.locals.remove(&name);
            }
        }
        self.numbered.truncate(numbered);
        self.family = family;
    }

    pub(crate) fn bind(&mut self, name: &str) {
        if name != "_" {
            *self.locals.entry(name.to_owned()).or_default() += 1;
            self.introduced.push(name.to_owned());
        }
    }

    pub(crate) fn placeholder(&mut self, stem: &str) {
        self.next_family += 1;
        self.family = self.next_family;
        self.numbered.push((stem.to_owned(), self.family));
    }

    pub(crate) fn pattern(&mut self, pattern: &ParamPattern) {
        if let Some(numbered) = &pattern.numbered {
            self.numbered
                .push((numbered.stem.name.clone(), self.family));
            return;
        }
        for elem in &pattern.elems {
            match elem {
                ParamPatternElem::Bind { name, .. } => self.bind(name),
                ParamPatternElem::Tuple(inner) => self.pattern(inner),
                ParamPatternElem::BindTuple { name, inner, .. } => {
                    self.bind(name);
                    self.pattern(inner);
                }
            }
        }
    }

    pub(crate) fn binding(&mut self, name: &str, pattern: Option<&ParamPattern>) {
        self.bind(name);
        if let Some(pattern) = pattern {
            self.pattern(pattern);
        }
    }
}

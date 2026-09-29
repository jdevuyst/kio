//! Source-facing names for alpha-normalized lexical binders.
//!
//! Presentation accompanies a normalized tree but never validates one.

use std::collections::HashMap;

use crate::span::Span;

/// Lexical names and their declaration origins at an observation's check site.
/// Manually constructed name-only facts retain membership without claiming an origin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg(feature = "surface")]
pub struct TypeBinderScope(pub(crate) HashMap<String, Option<Span>>);

#[cfg(feature = "surface")]
impl From<std::collections::HashSet<String>> for TypeBinderScope {
    fn from(names: std::collections::HashSet<String>) -> Self {
        Self(names.into_iter().map(|name| (name, None)).collect())
    }
}

#[cfg(feature = "surface")]
impl TypeBinderScope {
    pub(crate) fn names(&self) -> std::collections::HashSet<String> {
        self.0.keys().cloned().collect()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BinderPresentation {
    locations: HashMap<NameLocation, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NameLocation {
    module_path: String,
    span: Span,
    normalized: String,
}

impl BinderPresentation {
    pub(super) fn record_name(
        &mut self,
        module_path: &str,
        span: Span,
        normalized: &str,
        source: &str,
    ) {
        let location = NameLocation {
            module_path: module_path.to_owned(),
            span,
            normalized: normalized.to_owned(),
        };
        self.locations.insert(location, source.to_owned());
    }

    pub(crate) fn extend(&mut self, other: &Self) {
        for (location, source) in &other.locations {
            self.record_name(
                &location.module_path,
                location.span,
                &location.normalized,
                source,
            );
        }
    }

    #[cfg(all(test, feature = "surface", feature = "lsp"))]
    pub(crate) fn storage_counts(&self) -> (usize, usize, usize) {
        (
            self.locations.len(),
            self.locations.capacity(),
            self.locations
                .iter()
                .map(|(key, value)| {
                    key.module_path.capacity() + key.normalized.capacity() + value.capacity()
                })
                .sum(),
        )
    }

    #[cfg(feature = "surface")]
    pub(crate) fn present_name(&self, module_path: &str, position: Span, name: &mut String) {
        if let Some(source) = self.source_name(module_path, position, name) {
            *name = source.to_owned();
        }
    }

    #[cfg(feature = "surface")]
    pub(crate) fn source_name(
        &self,
        module_path: &str,
        span: Span,
        normalized: &str,
    ) -> Option<&str> {
        self.locations
            .get(&NameLocation {
                module_path: module_path.to_owned(),
                span,
                normalized: normalized.to_owned(),
            })
            .map(String::as_str)
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;

    #[test]
    fn unrelated_module_at_same_span_cannot_perturb_exact_name_presentation() {
        let span = Span::new(17, 20);
        let mut presentation = BinderPresentation::default();
        presentation.record_name("provider_a", span, "A_n2", "A");
        presentation.record_name("provider_b", span, "A_n2", "Z");

        let mut provider_a_name = "A_n2".to_owned();
        presentation.present_name("provider_a", span, &mut provider_a_name);
        assert_eq!(provider_a_name, "A");

        let mut provider_b_name = "A_n2".to_owned();
        presentation.present_name("provider_b", span, &mut provider_b_name);
        assert_eq!(provider_b_name, "Z");
    }
}

//! Scalar configuration fields share their value kinds and omitted defaults.

#[cfg(feature = "cli")]
use crate::ast::BlockFieldValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldDefault {
    Unit,
    String(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigField {
    pub name: &'static str,
    pub default: Option<FieldDefault>,
    pub named_choices: &'static [&'static str],
}

impl ConfigField {
    pub const fn string(name: &'static str) -> Self {
        Self {
            name,
            default: None,
            named_choices: &[],
        }
    }

    pub const fn optional(name: &'static str, default: FieldDefault) -> Self {
        Self {
            name,
            default: Some(default),
            named_choices: &[],
        }
    }

    pub fn accepts_unit(self) -> bool {
        self.default.is_some()
    }

    pub const fn with_named_choices(mut self, choices: &'static [&'static str]) -> Self {
        self.named_choices = choices;
        self
    }

    pub fn accepts_string(self) -> bool {
        self.named_choices.is_empty()
    }

    pub fn accepts_word(self, value: &str) -> bool {
        self.named_choices.contains(&value)
    }

    /// Describe configuration forms, not a module's resolved Kio type.
    pub fn value_description(self) -> String {
        let mut forms = Vec::new();
        if self.accepts_unit() {
            forms.push("()".to_owned());
        }
        if self.named_choices.is_empty() {
            forms.push("String".to_owned());
        } else {
            forms.extend(self.named_choices.iter().map(|value| (*value).to_owned()));
        }
        forms.join(" | ")
    }

    #[cfg(feature = "lsp")]
    pub fn default_description(self) -> Option<String> {
        self.default.map(|default| match default {
            FieldDefault::Unit => "()".to_owned(),
            FieldDefault::String(value) => format!("{value:?}"),
        })
    }

    #[cfg(feature = "lsp")]
    pub fn value_choices(self) -> Vec<String> {
        let mut choices = Vec::new();
        if self.accepts_unit() {
            choices.push("()".to_owned());
        }
        choices.extend(self.named_choices.iter().map(|choice| (*choice).to_owned()));
        if let Some(default) = self.default_description()
            && !choices.contains(&default)
        {
            choices.push(default);
        }
        choices
    }

    #[cfg(feature = "cli")]
    pub fn default_value(self) -> Option<BlockFieldValue> {
        self.default.map(|default| match default {
            FieldDefault::Unit => BlockFieldValue::Unit,
            FieldDefault::String(value) => BlockFieldValue::Str(value.to_owned()),
        })
    }

    #[cfg(all(feature = "surface", feature = "cli"))]
    pub fn string_default(self) -> &'static str {
        match self.default {
            Some(FieldDefault::String(value)) => value,
            _ => unreachable!("the configuration field has a concrete string default"),
        }
    }

    #[cfg(all(feature = "surface", feature = "cli"))]
    pub fn resolve_string(self, value: Option<&BlockFieldValue>) -> &str {
        match value {
            Some(BlockFieldValue::Str(value)) => value,
            None | Some(BlockFieldValue::Unit) => self.string_default(),
            _ => unreachable!("the parser validates this string configuration field"),
        }
    }
}

pub(crate) const CACHE: ConfigField = ConfigField::optional("cache", FieldDefault::Unit);
pub(crate) const MD_OUT: ConfigField =
    ConfigField::optional("md_out", FieldDefault::String("out/docs-md/"));
pub(crate) const HTML: ConfigField =
    ConfigField::optional("html", FieldDefault::String("out/docs/"));
pub(crate) const GIT_PATH: ConfigField = ConfigField::optional("path", FieldDefault::Unit);
pub(crate) const DOCS_FIELDS: &[ConfigField] = &[
    ConfigField::string("md"),
    ConfigField::string("support"),
    MD_OUT,
    HTML,
];

pub(crate) const SOURCE_FIELDS: &[ConfigField] = &[
    GIT_PATH,
    ConfigField::string("git"),
    ConfigField::string("ref"),
];
pub(crate) const RESOLVED_FIELDS: &[ConfigField] = &[
    ConfigField::string("git"),
    ConfigField::string("ref"),
    GIT_PATH,
    ConfigField::string("commit"),
    ConfigField::string("sig"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFieldOwner {
    Build,
    Docs,
    Source,
    Resolved,
}

impl ConfigFieldOwner {
    pub fn fields(self) -> &'static [ConfigField] {
        match self {
            Self::Build => &[CACHE],
            Self::Docs => DOCS_FIELDS,
            Self::Source => SOURCE_FIELDS,
            Self::Resolved => RESOLVED_FIELDS,
        }
    }

    pub fn field(self, name: &str) -> Option<ConfigField> {
        self.fields()
            .iter()
            .copied()
            .find(|field| field.name == name)
    }
}

#[cfg(all(test, feature = "surface", feature = "cli"))]
mod tests {
    use super::*;

    #[test]
    fn optional_string_settings_select_the_same_default_for_omission_and_unit() {
        for (field, expected) in [(MD_OUT, "out/docs-md/"), (HTML, "out/docs/")] {
            assert!(field.accepts_unit());
            assert_eq!(field.resolve_string(None), expected);
            assert_eq!(field.resolve_string(Some(&BlockFieldValue::Unit)), expected);
            let explicit = BlockFieldValue::Str("custom".to_owned());
            assert_eq!(field.resolve_string(Some(&explicit)), "custom");
            let pinned = BlockFieldValue::Str(expected.to_owned());
            assert_eq!(field.resolve_string(Some(&pinned)), expected);
        }
    }
}

//! Spelling-defined Kio identifier roles and their diagnostic repairs.
//!
//! These predicates classify source spellings only. Resolution must not make
//! the role of a spelling depend on which declarations happen to be present.

use crate::error::{Error, Fix, FixEdit};
use crate::span::Span;

pub(crate) const TYPE_NAME_PATTERN: &str = "_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*";
pub(crate) const VALUE_NAME_PATTERN: &str = "_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameRole {
    Value,
    Type,
    Label,
    Module,
    Package,
    Dependency,
}

impl NameRole {
    fn description(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Type => "type",
            Self::Label => "label",
            Self::Module => "module",
            Self::Package => "package",
            Self::Dependency => "dependency",
        }
    }

    fn pattern(self) -> &'static str {
        match self {
            Self::Type => TYPE_NAME_PATTERN,
            Self::Label => "[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*",
            _ => VALUE_NAME_PATTERN,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameViolation {
    MissingLetter,
    ReservedPrefix,
    LeadingUnderscore,
    WrongInitialCase,
    LaterUppercase,
    WordStartsWithDigit,
    LetterAfterDigit,
    ConsecutiveSeparators,
    InvalidCharacter,
}

impl NameViolation {
    pub(crate) fn explanation(self, role: NameRole) -> &'static str {
        match self {
            Self::MissingLetter => "must contain at least one ASCII letter",
            Self::ReservedPrefix => {
                "may not begin with `__` (reserved for compiler-generated names)"
            }
            Self::LeadingUnderscore => "may not begin with `_`",
            Self::WrongInitialCase if role == NameRole::Type => {
                "must have an uppercase first letter"
            }
            Self::WrongInitialCase => "must have a lowercase first letter",
            Self::LaterUppercase => "must use lowercase letters after its first letter",
            Self::WordStartsWithDigit => "must start every underscore-separated word with a letter",
            Self::LetterAfterDigit => "must separate a letter following a digit with `_`",
            Self::ConsecutiveSeparators => "must separate internal words with exactly one `_`",
            Self::InvalidCharacter => "may contain only ASCII letters, digits, and `_`",
        }
    }
}

fn validate_shape(name: &str, role: NameRole) -> Result<(), NameViolation> {
    if !name.bytes().any(|byte| byte.is_ascii_alphabetic()) {
        return Err(NameViolation::MissingLetter);
    }
    let body = name.trim_matches('_');
    let mut first_letter = true;
    let mut word_start = true;
    let mut after_digit = false;
    for byte in body.bytes() {
        match byte {
            b'_' => {
                if word_start {
                    return Err(NameViolation::ConsecutiveSeparators);
                }
                word_start = true;
                after_digit = false;
            }
            b'0'..=b'9' => {
                if word_start {
                    return Err(NameViolation::WordStartsWithDigit);
                }
                after_digit = true;
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                if first_letter {
                    if byte.is_ascii_uppercase() != (role == NameRole::Type) {
                        return Err(NameViolation::WrongInitialCase);
                    }
                } else if byte.is_ascii_uppercase() {
                    return Err(NameViolation::LaterUppercase);
                }
                if after_digit {
                    return Err(NameViolation::LetterAfterDigit);
                }
                first_letter = false;
                word_start = false;
            }
            _ => return Err(NameViolation::InvalidCharacter),
        }
    }
    Ok(())
}

pub(crate) fn validate_user_name(name: &str, role: NameRole) -> Result<(), NameViolation> {
    if name.starts_with("__") {
        return Err(NameViolation::ReservedPrefix);
    }
    if role == NameRole::Label && name.starts_with('_') {
        return Err(NameViolation::LeadingUnderscore);
    }
    validate_shape(name, role)
}

/// Whether `name` begins like a type reference, including reserved prefixes.
///
/// The complete suffix is deliberately not checked here: parser
/// disambiguation uses this predicate before the role-specific validator can
/// produce the precise diagnostic for a malformed suffix.
pub(crate) fn starts_like_type_name(name: &str) -> bool {
    let body = name.trim_start_matches('_');
    body.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

/// Whether `name` is a complete user type name.
pub(crate) fn is_type_name(name: &str) -> bool {
    validate_user_name(name, NameRole::Type).is_ok()
}

/// References admit reserved hygiene prefixes, but retain their exact role.
pub(crate) fn is_type_reference_name(name: &str) -> bool {
    validate_shape(name, NameRole::Type).is_ok()
}

pub(crate) fn is_value_reference_name(name: &str) -> bool {
    validate_shape(name, NameRole::Value).is_ok()
}

/// Whether `name` is one of the reserved compiler-generated spellings that a
/// phase artifact may reference. Like every name role, the spelling must
/// contain a letter. Reservation supplies hygiene, not semantic privilege;
/// consumers still validate the referenced declaration normally.
pub(crate) fn is_compiler_reserved_name(name: &str) -> bool {
    name.starts_with("__")
        && (validate_shape(name, NameRole::Value).is_ok()
            || validate_shape(name, NameRole::Type).is_ok())
}

/// Whether `name` is a complete user value name.
pub(crate) fn is_value_name(name: &str) -> bool {
    validate_user_name(name, NameRole::Value).is_ok()
}

/// Whether `name` belongs to either user-spellable identifier namespace.
pub(crate) fn is_user_value_or_type_name(name: &str) -> bool {
    is_value_name(name) || is_type_name(name)
}

/// Encode identity bytes into one lowercase, letter-only source word.
/// Two letters per byte preserve exact identity without introducing a digit
/// followed by a letter or an underscore that could join a source affix.
pub(crate) fn encode_name_component(identity: &str) -> String {
    let mut encoded = String::with_capacity(identity.len() * 2);
    for byte in identity.bytes() {
        encoded.push(char::from(b'a' + (byte >> 4)));
        encoded.push(char::from(b'a' + (byte & 15)));
    }
    encoded
}

/// Add a numeric freshness word before the original trailing affix.
pub(crate) fn indexed_name(base: &str, index: impl std::fmt::Display) -> String {
    let body = base.trim_end_matches('_');
    format!("{body}_n{index}{}", &base[body.len()..])
}

pub(crate) fn name_repairs(name: &str, role: NameRole) -> Vec<String> {
    if validate_user_name(name, role).is_ok()
        || !name.bytes().any(|byte| byte.is_ascii_alphabetic())
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Vec::new();
    }
    let body = name.trim_matches('_');
    if !body.as_bytes()[0].is_ascii_alphabetic() {
        return Vec::new();
    }
    let mut repaired = String::new();
    let mut separator = false;
    let mut after_digit = false;
    for byte in body.bytes() {
        if byte == b'_' {
            separator = true;
            continue;
        }
        if byte.is_ascii_alphabetic() {
            if !repaired.is_empty() && (separator || after_digit) {
                repaired.push('_');
            }
            let letter = if repaired.is_empty() && role == NameRole::Type {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            };
            repaired.push(char::from(letter));
            after_digit = false;
        } else {
            repaired.push(char::from(byte));
            after_digit = true;
        }
        separator = false;
    }
    let trailing = name.len() - name.trim_end_matches('_').len();
    repaired.extend(std::iter::repeat_n('_', trailing));
    let mut candidates = Vec::new();
    if name.starts_with('_') && role != NameRole::Label {
        candidates.push(format!("_{repaired}"));
    }
    if !name.starts_with('_') || name.starts_with("__") || role == NameRole::Label {
        candidates.push(repaired);
    }
    candidates.retain(|candidate| validate_user_name(candidate, role).is_ok());
    candidates
}

pub(crate) fn validate_source_name(name: &str, role: NameRole, span: Span) -> Result<(), Error> {
    let Err(violation) = validate_user_name(name, role) else {
        return Ok(());
    };
    let mut error = Error::parse(
        span,
        format!(
            "{} name `{name}` {}",
            role.description(),
            violation.explanation(role)
        ),
    )
    .with_note(format!(
        "{} names match {}",
        role.description(),
        role.pattern()
    ));
    for replacement in name_repairs(name, role) {
        error = error.with_fix(Fix::maybe_incorrect(
            format!("Replace this token with `{replacement}`"),
            vec![FixEdit::new(span, replacement)],
        ));
    }
    Err(error)
}

pub(crate) fn validate_reference_name(name: &str, role: NameRole, span: Span) -> Result<(), Error> {
    if !name.starts_with("__") {
        return validate_source_name(name, role, span);
    }
    validate_shape(name, role).map_err(|violation| {
        Error::parse(
            span,
            format!(
                "{} name `{name}` {}",
                role.description(),
                violation.explanation(role)
            ),
        )
        .with_note("reserved names follow the ordinary word and type/value role rules")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_components_and_indices_preserve_source_roles() {
        let mut encoded = std::collections::BTreeSet::new();
        for identity in ["", "a", "a/b", "a_b", "_a", "a_", "a1", "é"] {
            let word = encode_name_component(identity);
            assert!(word.bytes().all(|byte| byte.is_ascii_lowercase()));
            assert!(encoded.insert(word));
        }
        for base in ["a", "a1_b2", "_a", "a_", "a__", "A", "_A", "A__"] {
            let indexed = indexed_name(base, 12);
            assert_eq!(is_value_name(base), is_value_name(&indexed), "{indexed}");
            assert_eq!(is_type_name(base), is_type_name(&indexed), "{indexed}");
        }
        assert_eq!(indexed_name("_A__", 2), "_A_n2__");
        assert!(is_compiler_reserved_name(&indexed_name("__value__", 2)));
    }

    #[test]
    fn type_name_contract_has_one_optional_marker() {
        for name in [
            "Foo",
            "Foo_bar",
            "I32",
            "Foo123_bar4",
            "A",
            "_Foo",
            "_Foo_bar",
            "_A",
            "Foo__",
        ] {
            assert!(is_type_name(name), "{name}");
        }
        for name in [
            "foo", "_foo", "_1Foo", "__Foo", "__Foo__", "FooBar", "_FooBar", "Foo_123", "Foo__bar",
            "FOO", "_", "",
        ] {
            assert!(!is_type_name(name), "{name}");
        }
    }

    #[test]
    fn type_head_classification_is_spelling_only() {
        for name in ["Foo", "FooBar", "_Foo", "_FooBar", "__Foo", "___Foo1Bar"] {
            assert!(starts_like_type_name(name), "{name}");
        }
        for name in ["foo", "_foo", "_1Foo", ""] {
            assert!(!starts_like_type_name(name), "{name}");
        }
    }

    #[test]
    fn value_name_contract_keeps_lowercase_marked_names_distinct() {
        for name in [
            "foo",
            "foo_bar",
            "_foo",
            "a1_b2",
            "foo123_bar4",
            "sha256",
            "p0",
            "foo__",
        ] {
            assert!(is_value_name(name), "{name}");
        }
        for name in [
            "Foo",
            "_Foo",
            "_1Foo",
            "__foo",
            "a1b",
            "foo_123",
            "foo__bar",
            "foo_1_bar",
            "_1foo",
            "_1",
            "_",
            "",
        ] {
            assert!(!is_value_name(name), "{name}");
        }
    }

    #[test]
    fn ambiguous_user_name_role_is_an_exact_union() {
        for name in ["value", "_value", "value1", "Type", "_Type"] {
            assert!(is_user_value_or_type_name(name), "{name}");
        }
        for name in ["_FooBar", "_1Foo", "FooBar", "__Foo", "_"] {
            assert!(!is_user_value_or_type_name(name), "{name}");
        }
        assert!(is_compiler_reserved_name("__Type__"));
        assert!(is_compiler_reserved_name("__value__"));
        assert!(is_compiler_reserved_name("__Foo"));
        assert!(is_compiler_reserved_name("___Checked_term"));
        assert!(is_compiler_reserved_name("___x1_y2___"));
        for name in [
            "__",
            "___",
            "____",
            "__1__",
            "__a1b",
            "__Foo__bar",
            "__foo_123",
        ] {
            assert!(!is_compiler_reserved_name(name), "{name}");
        }
    }

    #[test]
    fn reserved_reference_names_keep_the_ordinary_word_and_case_rules() {
        for name in ["__Type", "___Item_type1__"] {
            assert!(is_type_reference_name(name), "{name}");
            assert!(!is_value_reference_name(name), "{name}");
            assert!(!is_type_name(name), "{name}");
            assert!(validate_reference_name(name, NameRole::Type, Span::new(0, 0)).is_ok());
        }
        for name in ["__value", "___item_value1__"] {
            assert!(is_value_reference_name(name), "{name}");
            assert!(!is_type_reference_name(name), "{name}");
            assert!(!is_value_name(name), "{name}");
            assert!(validate_reference_name(name, NameRole::Value, Span::new(0, 0)).is_ok());
        }
        for name in ["__", "__1__", "__foo_123", "__FooBar", "__Foo__bar"] {
            assert!(!is_type_reference_name(name), "{name}");
            assert!(!is_value_reference_name(name), "{name}");
        }
    }

    #[test]
    fn repairs_obey_the_diagnosed_role() {
        for (name, role, expected) in [
            ("a1b", NameRole::Value, vec!["a1_b"]),
            ("foo123bar", NameRole::Value, vec!["foo123_bar"]),
            ("foo_123", NameRole::Value, vec!["foo123"]),
            ("foo__bar", NameRole::Value, vec!["foo_bar"]),
            ("Foo_Bar", NameRole::Type, vec!["Foo_bar"]),
            ("foo_Bar", NameRole::Value, vec!["foo_bar"]),
            ("__foo", NameRole::Value, vec!["_foo", "foo"]),
            ("_foo", NameRole::Label, vec!["foo"]),
            ("_Foo", NameRole::Type, vec![]),
            ("_123", NameRole::Value, vec![]),
        ] {
            assert_eq!(name_repairs(name, role), expected, "{name}");
        }
    }
}

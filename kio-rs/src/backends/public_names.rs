//! Shared semantic identities for generated host-facing names.
//!
//! Backends render these identities using their host language's identifier
//! rules. Readable components retain source word boundaries through casing;
//! exact frames preserve affixes, paths, and semantic roles separately.

/// Render one Kio component with recoverable word boundaries. Leading and
/// trailing underscores retain their count and side; internal separators are
/// recovered from the uppercase initials they introduce.
pub(crate) fn host_name_core(source: &str) -> String {
    let leading = source.len() - source.trim_start_matches('_').len();
    let body = source.trim_matches('_');
    if body.is_empty() {
        return source.to_owned();
    }
    let trailing = source.len() - source.trim_end_matches('_').len();
    let mut rendered = String::with_capacity(source.len());
    rendered.extend(std::iter::repeat_n('_', leading));
    for (index, word) in body.split('_').enumerate() {
        if index == 0 {
            rendered.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                rendered.push(first.to_ascii_uppercase());
                rendered.push_str(chars.as_str());
            }
        }
    }
    rendered.extend(std::iter::repeat_n('_', trailing));
    rendered
}

/// Case each component before exact path encoding; neither separators nor
/// semantic identity bytes are interpreted as source word boundaries.
pub(crate) fn encode_host_identity(source: &str) -> String {
    encode_source_identity(
        &source
            .split('/')
            .map(host_name_core)
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// One namespace edge in a generated package facade.
///
/// Kio keeps module, type, and value names in distinct namespaces. Host
/// languages frequently do not, so the role must remain part of the generated
/// selector instead of being discarded while a flat tree is assembled.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum FacadeSelector {
    Module(String),
    Type(String),
}

impl FacadeSelector {
    pub(crate) fn source(&self) -> &str {
        match self {
            Self::Module(source) | Self::Type(source) => source,
        }
    }

    /// The exact host-facing selector before target-language escaping.
    ///
    /// A root module uses its readable component. Nested module and type edges
    /// use disjoint reserved classes. Encoding the full source component
    /// prevents source underscores from colliding with generated separators.
    pub(crate) fn facade_name(&self, root: bool) -> String {
        match self {
            Self::Module(source) if root => host_name_core(source),
            Self::Module(source) => {
                format!("KioModule_{}", encode_host_identity(source))
            }
            Self::Type(source) => format!("KioType_{}", encode_host_identity(source)),
        }
    }
}

/// Split a canonical slash-separated module path into role-bearing facade
/// selectors.
pub(crate) fn module_facade_path(path: &str) -> Vec<FacadeSelector> {
    path.split('/')
        .map(|segment| FacadeSelector::Module(segment.to_owned()))
        .collect()
}

/// Encode a Kio identifier or canonical module path without losing source
/// separators.
pub(crate) fn encode_source_identity(source: &str) -> String {
    let mut encoded = String::with_capacity(source.len());
    for byte in source.bytes() {
        match byte {
            b'_' => encoded.push_str("_u"),
            b'/' => encoded.push_str("_s"),
            _ => encoded.push(char::from(byte)),
        }
    }
    encoded
}

/// Readable declaration identity for a role-adapter method.
///
/// Unmarked source components can be joined without losing their
/// boundaries. The exact `V1_…` frame owns its leading class, so declarations
/// that could enter that class (or whose components need escaping) use the
/// exact fallback instead. This choice is per identity and therefore cannot
/// change when another declaration is added.
pub(crate) fn readable_role_adapter_identity(
    module_segments: &[String],
    name: &str,
) -> Option<String> {
    let components = module_segments
        .iter()
        .map(|component| host_name_core(component))
        .chain(std::iter::once(host_name_core(name)))
        .collect::<Vec<_>>();
    if module_segments.is_empty()
        || components.first().is_some_and(|segment| segment == "V1")
        || components.iter().any(|component| {
            component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return None;
    }
    Some(components.join("_"))
}

/// Exact public key for one module's host-record namespace.
///
/// Slash-only paths join readable components with `_`. A source
/// underscore selects a disjoint exact class so `foo/bar` and `foo_bar` can
/// coexist.
pub(crate) fn host_module_key(module_path: &str) -> String {
    if !module_path.contains('_') {
        module_path
            .split('/')
            .map(host_name_core)
            .collect::<Vec<_>>()
            .join("_")
    } else {
        format!("KioModule_{}", encode_host_identity(module_path))
    }
}

/// Whether every source underscore introduces a non-empty lowercase snake
/// component. Case-converting backends may keep their idiomatic readable
/// spelling exactly for this injective subset.
pub(crate) fn has_readable_snake_components(source: &str) -> bool {
    let bytes = source.as_bytes();
    !bytes.is_empty()
        && bytes[0] != b'_'
        && bytes.iter().enumerate().all(|(index, byte)| {
            *byte != b'_' || bytes.get(index + 1).is_some_and(u8::is_ascii_lowercase)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_word_casing_retains_affixes_and_has_a_unique_inverse() {
        for (source, rendered) in [
            ("foo123_bar4", "foo123Bar4"),
            ("Foo123_bar4", "Foo123Bar4"),
            ("_foo_bar__", "_fooBar__"),
            ("_Foo_bar__", "_FooBar__"),
        ] {
            assert_eq!(host_name_core(source), rendered);
        }
        for number in 0usize..5usize.pow(7) {
            let mut remainder = number;
            let mut source = String::new();
            for _ in 0..7 {
                source.push(char::from(b"ab01_"[remainder % 5]));
                remainder /= 5;
            }
            if !crate::naming::is_value_name(&source) {
                continue;
            }
            let rendered = host_name_core(&source);
            let mut recovered = String::new();
            for ch in rendered.chars() {
                if ch.is_ascii_uppercase() {
                    recovered.push('_');
                    recovered.push(ch.to_ascii_lowercase());
                } else {
                    recovered.push(ch);
                }
            }
            assert_eq!(recovered, source);
        }
        assert_ne!(encode_host_identity("a_/b"), encode_host_identity("a/_b"));
        assert_ne!(
            encode_host_identity("foo/bar"),
            encode_host_identity("foo_bar")
        );
    }

    #[test]
    fn cosmetic_word_casing_is_total_without_duplicating_affixes() {
        for (source, rendered) in [
            ("", ""),
            ("_", "_"),
            ("___", "___"),
            ("word_éclair", "wordéclair"),
            ("__éclair_", "__éclair_"),
        ] {
            assert_eq!(host_name_core(source), rendered);
        }
    }

    #[test]
    fn host_module_keys_distinguish_source_underscores_from_slashes() {
        assert_eq!(host_module_key("foo/bar"), "foo_bar");
        assert_eq!(host_module_key("foo_bar"), "KioModule_fooBar");
        assert_ne!(host_module_key("foo/bar"), host_module_key("foo_bar"));
    }

    #[test]
    fn facade_roles_have_disjoint_exact_selectors() {
        let module = FacadeSelector::Module("child".to_owned());
        let ty = FacadeSelector::Type("child".to_owned());
        assert_eq!(module.facade_name(false), "KioModule_child");
        assert_eq!(ty.facade_name(false), "KioType_child");
        assert_ne!(module.facade_name(false), ty.facade_name(false));
    }

    #[test]
    fn marked_type_selector_preserves_its_exact_source_identity() {
        let ordinary = FacadeSelector::Type("Box".to_owned()).facade_name(false);
        let marked = FacadeSelector::Type("_Box".to_owned()).facade_name(false);
        assert_eq!(ordinary, "KioType_Box");
        assert_eq!(marked, "KioType__uBox");
        assert_ne!(ordinary, marked);
    }

    #[test]
    fn role_adapter_identity_uses_readable_and_reserved_classes() {
        assert_eq!(
            readable_role_adapter_identity(&["api".to_owned()], "Count").as_deref(),
            Some("api_Count")
        );
        assert_eq!(
            readable_role_adapter_identity(&["foo_bar".to_owned()], "Count"),
            Some("fooBar_Count".to_owned())
        );
        assert_eq!(
            readable_role_adapter_identity(&["V1".to_owned()], "Count"),
            None
        );
    }
}

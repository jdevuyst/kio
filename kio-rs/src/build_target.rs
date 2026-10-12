//! Build-target vocabulary shared by dispatch, configuration, and tooling.

use crate::config_fields::{ConfigField, FieldDefault};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTarget {
    Js,
    Ts,
    Go,
    Python,
    Java,
    Swift,
    Haskell,
    Rust,
    KioPrime,
}

impl BuildTarget {
    pub const ALL: &'static [Self] = &[
        Self::Js,
        Self::Ts,
        Self::Go,
        Self::Python,
        Self::Java,
        Self::Swift,
        Self::Haskell,
        Self::Rust,
        Self::KioPrime,
    ];

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|target| target.id() == id)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Js => "js",
            Self::Ts => "ts",
            Self::Go => "go",
            Self::Python => "python",
            Self::Java => "java",
            Self::Swift => "swift",
            Self::Haskell => "haskell",
            Self::Rust => "rust",
            Self::KioPrime => "kio-prime",
        }
    }

    pub fn fields(self) -> &'static [ConfigField] {
        const OUT: ConfigField = ConfigField::string("out");
        const NAMESPACE: ConfigField = ConfigField::optional("namespace", FieldDefault::Unit);
        match self {
            Self::KioPrime => &[OUT],
            Self::Rust => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        ConfigField::optional("thread_safety", FieldDefault::Unit)
                            .with_named_choices(&["send", "sync", "send_sync"]),
                    ]
                }
            }
            _ => &[OUT, NAMESPACE],
        }
    }

    #[cfg(any(test, feature = "cli"))]
    pub fn keys(self) -> impl Iterator<Item = &'static str> {
        self.fields().iter().map(|field| field.name)
    }

    pub fn field(self, name: &str) -> Option<ConfigField> {
        self.fields()
            .iter()
            .copied()
            .find(|field| field.name == name)
    }

    #[cfg(feature = "cli")]
    pub fn default_namespace(self, package: &str) -> String {
        (self.namespace_policy().derive)(package)
    }

    /// A written Unit selects the current package-derived namespace, just like
    /// omission. An explicit string remains an override across package renames.
    #[cfg(feature = "cli")]
    pub fn namespace_override(
        self,
        target: &crate::ast::TargetBlock,
    ) -> Result<Option<&str>, String> {
        let mut fields = target
            .entries
            .iter()
            .filter(|entry| entry.key == "namespace");
        let value = fields.next();
        if fields.next().is_some() {
            return Err("duplicate key `namespace`".to_owned());
        }
        match value.map(|entry| &entry.value) {
            None | Some(crate::ast::BlockFieldValue::Unit) => Ok(None),
            Some(crate::ast::BlockFieldValue::Str(value)) => {
                (self.namespace_policy().validate)(value)?;
                Ok(Some(value))
            }
            _ => unreachable!("the parser validates namespace field value kinds"),
        }
    }

    #[cfg(feature = "cli")]
    fn namespace_policy(self) -> NamespacePolicy {
        use crate::backends::namespace::*;
        match self {
            Self::Js | Self::Ts => {
                NamespacePolicy::new(default_js_namespace, validate_js_namespace)
            }
            Self::Go => NamespacePolicy::new(default_go_namespace, validate_go_namespace),
            Self::Python => {
                NamespacePolicy::new(default_python_namespace, validate_python_namespace)
            }
            Self::Java => NamespacePolicy::new(default_java_namespace, validate_java_namespace),
            Self::Swift => NamespacePolicy::new(default_swift_namespace, validate_swift_namespace),
            Self::Haskell => {
                NamespacePolicy::new(default_haskell_namespace, validate_haskell_namespace)
            }
            Self::Rust => NamespacePolicy::new(default_rust_namespace, validate_rust_namespace),
            Self::KioPrime => unreachable!("the Kio' target has no artifact namespace"),
        }
    }

    #[cfg(feature = "cli")]
    pub(crate) fn key(self, name: &str) -> Option<&'static str> {
        self.keys().find(|key| *key == name)
    }
}

#[cfg(feature = "cli")]
struct NamespacePolicy {
    derive: fn(&str) -> String,
    validate: fn(&str) -> Result<(), String>,
}

#[cfg(feature = "cli")]
impl NamespacePolicy {
    fn new(derive: fn(&str) -> String, validate: fn(&str) -> Result<(), String>) -> Self {
        Self { derive, validate }
    }
}

#[cfg(test)]
mod tests {
    use super::BuildTarget;

    #[test]
    fn catalog_preserves_existing_dispatch_ids_and_key_boundaries() {
        let expected = [
            ("js", &["out", "namespace"][..]),
            ("ts", &["out", "namespace"][..]),
            ("go", &["out", "namespace"][..]),
            ("python", &["out", "namespace"][..]),
            ("java", &["out", "namespace"][..]),
            ("swift", &["out", "namespace"][..]),
            ("haskell", &["out", "namespace"][..]),
            ("rust", &["out", "namespace", "thread_safety"][..]),
            ("kio-prime", &["out"][..]),
        ];
        assert_eq!(BuildTarget::ALL.len(), expected.len());
        for (target, (id, keys)) in BuildTarget::ALL.iter().copied().zip(expected) {
            assert_eq!(target.id(), id);
            assert_eq!(BuildTarget::from_id(id), Some(target));
            assert_eq!(target.keys().collect::<Vec<_>>(), keys);
            assert!(!target.keys().any(|key| key == "crate_name"));
            #[cfg(feature = "cli")]
            {
                for key in keys {
                    assert_eq!(target.key(key), Some(*key));
                }
                assert_eq!(target.key("crate_name"), None);
                assert_eq!(target.key("unknown"), None);
            }
        }
        for unknown in ["", "JS", "prime", "unknown"] {
            assert_eq!(BuildTarget::from_id(unknown), None);
        }
    }

    #[cfg(all(feature = "cli", feature = "surface"))]
    #[test]
    fn namespace_selection_preserves_absence_and_explicit_overrides() {
        use crate::pass::parser::parse_package_file;
        for backend in BuildTarget::ALL
            .iter()
            .copied()
            .filter(|b| *b != BuildTarget::KioPrime)
        {
            let explicit = if matches!(backend, BuildTarget::Swift | BuildTarget::Haskell) {
                "Pinned"
            } else {
                "pinned"
            };
            for setting in [
                String::new(),
                "namespace ((()))".to_owned(),
                format!("namespace {explicit:?}"),
            ] {
                for name in ["before", "after"] {
                    let source = format!(
                        "package {name}; build {{ target {} {{ out \"out\"; {setting} }} }}",
                        backend.id()
                    );
                    let package = parse_package_file(&source, Some(name)).unwrap();
                    let target = &package.build.as_ref().unwrap().targets[0];
                    let selected = backend.namespace_override(target).unwrap();
                    if setting.contains('"') {
                        assert_eq!(selected, Some(explicit), "{}", backend.id());
                    } else {
                        assert_eq!(selected, None, "{}", backend.id());
                        assert_ne!(
                            backend.default_namespace("before"),
                            backend.default_namespace("after")
                        );
                    }
                }
            }
            for setting in [
                "namespace (); namespace ()",
                "namespace (); namespace \"pinned\"",
                "namespace \"pinned\"; namespace ()",
            ] {
                let source = format!(
                    "package app; build {{ target {} {{ out \"out\"; {setting} }} }}",
                    backend.id()
                );
                let package = parse_package_file(&source, Some("app")).unwrap();
                assert_eq!(
                    backend.namespace_override(&package.build.unwrap().targets[0]),
                    Err("duplicate key `namespace`".to_owned())
                );
            }
            let source = format!(
                "package app; build {{ target {} {{ out \"out\"; namespace \"\" }} }}",
                backend.id()
            );
            let package = parse_package_file(&source, Some("app")).unwrap();
            assert!(
                backend
                    .namespace_override(&package.build.unwrap().targets[0])
                    .is_err()
            );
        }
        assert!(BuildTarget::KioPrime.field("namespace").is_none());
    }
}

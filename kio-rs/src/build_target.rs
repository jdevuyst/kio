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
        const BASELINE: ConfigField = ConfigField::optional("baseline", FieldDefault::Unit);
        match self {
            Self::KioPrime => &[OUT],
            Self::Js => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "es2020"]),
                    ]
                }
            }
            Self::Ts => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "v5_0"]),
                    ]
                }
            }
            Self::Go => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "v1_26"]),
                    ]
                }
            }
            Self::Python => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "v3_10"]),
                    ]
                }
            }
            Self::Java => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "v21"]),
                    ]
                }
            }
            Self::Swift => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "v6_0"]),
                    ]
                }
            }
            Self::Haskell => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "ghc9_10"]),
                    ]
                }
            }
            Self::Rust => {
                const {
                    &[
                        OUT,
                        NAMESPACE,
                        BASELINE.with_named_choices(&["latest", "v1_85"]),
                        ConfigField::optional("thread_safety", FieldDefault::Unit)
                            .with_named_choices(&["send", "sync", "send_sync"]),
                    ]
                }
            }
        }
    }

    /// Concrete baselines are ordered from oldest to newest in the field catalog.
    pub fn latest_baseline(self) -> Option<&'static str> {
        self.field("baseline")?.named_choices.last().copied()
    }

    #[cfg(feature = "cli")]
    pub fn resolve_baseline(
        self,
        target: &crate::ast::TargetBlock,
    ) -> Result<Option<&'static str>, String> {
        let mut entries = target
            .entries
            .iter()
            .filter(|entry| entry.key == "baseline");
        let value = entries.next();
        if entries.next().is_some() {
            return Err("duplicate key `baseline`".to_owned());
        }
        let Some(field) = self.field("baseline") else {
            return if value.is_none() {
                Ok(None)
            } else {
                Err(format!(
                    "the `{}` backend has no `baseline` setting",
                    self.id()
                ))
            };
        };
        match value.map(|entry| &entry.value) {
            None | Some(crate::ast::BlockFieldValue::Unit) => Ok(self.latest_baseline()),
            Some(crate::ast::BlockFieldValue::Word(value)) if value == "latest" => {
                Ok(self.latest_baseline())
            }
            Some(crate::ast::BlockFieldValue::Word(value)) => field
                .named_choices
                .iter()
                .copied()
                .find(|choice| *choice == value)
                .map(Some)
                .ok_or_else(|| {
                    format!(
                        "unknown baseline `{value}` for the `{}` backend (recognized: {})",
                        self.id(),
                        field.named_choices.join(", "),
                    )
                }),
            _ => unreachable!("the parser validates baseline field value kinds"),
        }
    }

    #[cfg(feature = "cli")]
    pub fn javascript_runtime_baseline(self, baseline: &str) -> &'static str {
        match (self, baseline) {
            (Self::Js, "es2020") | (Self::Ts, "v5_0") => "es2020",
            _ => unreachable!("JS emission receives a supported JS or TypeScript baseline"),
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
            ("js", &["out", "namespace", "baseline"][..]),
            ("ts", &["out", "namespace", "baseline"][..]),
            ("go", &["out", "namespace", "baseline"][..]),
            ("python", &["out", "namespace", "baseline"][..]),
            ("java", &["out", "namespace", "baseline"][..]),
            ("swift", &["out", "namespace", "baseline"][..]),
            ("haskell", &["out", "namespace", "baseline"][..]),
            (
                "rust",
                &["out", "namespace", "baseline", "thread_safety"][..],
            ),
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
    fn baselines_resolve_from_the_fixed_backend_catalog() {
        use crate::pass::parser::parse_package_file;
        for (backend, expected) in [
            (BuildTarget::Js, "es2020"),
            (BuildTarget::Ts, "v5_0"),
            (BuildTarget::Python, "v3_10"),
            (BuildTarget::Rust, "v1_85"),
            (BuildTarget::Go, "v1_26"),
            (BuildTarget::Java, "v21"),
            (BuildTarget::Swift, "v6_0"),
            (BuildTarget::Haskell, "ghc9_10"),
        ] {
            for setting in [
                String::new(),
                "baseline ((()))".to_owned(),
                "baseline ((latest))".to_owned(),
                format!("baseline ({expected})"),
            ] {
                let source = format!(
                    "package app; build {{ target ({}, \"chosen\") {{ out \"out\"; {setting} }} }}",
                    backend.id()
                );
                let package = parse_package_file(&source, Some("app")).unwrap();
                assert_eq!(
                    backend.resolve_baseline(&package.build.unwrap().targets[0]),
                    Ok(Some(expected))
                );
            }
            for setting in ["baseline unknown", "baseline (); baseline latest"] {
                let source = format!(
                    "package app; build {{ target {} {{ out \"out\"; {setting} }} }}",
                    backend.id()
                );
                let package = parse_package_file(&source, Some("app")).unwrap();
                assert!(
                    backend
                        .resolve_baseline(&package.build.unwrap().targets[0])
                        .is_err()
                );
            }
            let source = format!(
                "package app; build {{ target {} {{ out \"out\"; baseline \"{expected}\" }} }}",
                backend.id()
            );
            assert!(parse_package_file(&source, Some("app")).is_err());
        }
        assert_eq!(BuildTarget::KioPrime.latest_baseline(), None);
        for value in ["latest", "()", "es2020"] {
            let source = format!(
                "package app; build {{ target kio-prime {{ out \"out\"; baseline {value} }} }}"
            );
            assert!(parse_package_file(&source, Some("app")).is_err());
        }
        let prime = parse_package_file(
            "package app; build { target kio-prime { out \"out\"; baseline \"latest\" } }",
            Some("app"),
        )
        .unwrap();
        assert!(
            BuildTarget::KioPrime
                .resolve_baseline(&prime.build.unwrap().targets[0])
                .is_err()
        );
        assert_eq!(
            BuildTarget::Js.javascript_runtime_baseline("es2020"),
            BuildTarget::Ts.javascript_runtime_baseline("v5_0")
        );
        for source in [
            "package app; build { target rust { out \"out\"; thread_safety v1_85 } }",
            "package app; build { target js { out \"out\"; baseline v5_0 } }",
            "package app; build { target ts { out \"out\"; baseline v9_9 } }",
        ] {
            assert!(parse_package_file(source, Some("app")).is_err(), "{source}");
        }
        for source in [
            "module main; fn v5_0() {}",
            "module main; fn value() { v5_0 }",
        ] {
            assert!(crate::pass::parser::parse(source).is_err(), "{source}");
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

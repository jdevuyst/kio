//! Artifact addressing supplied independently of emitted backend source.
//!
//! The corpus harness supplies the Kio source package name and an optional,
//! already-selected backend namespace override. A runner derives its backend's
//! specified default namespace from the source name only when that override is
//! absent. The resulting effective namespace—not the package name—is backend
//! artifact identity. This is an independent ABI consumer: it deliberately
//! shares no compiler implementation.

const RUST_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];

const JAVA_KEYWORDS: &[&str] = &[
    "_",
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "void",
    "volatile",
    "while",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

const SWIFT_RESERVED: &[&str] = &[
    "associatedtype",
    "class",
    "deinit",
    "enum",
    "extension",
    "fileprivate",
    "func",
    "import",
    "init",
    "inout",
    "internal",
    "let",
    "open",
    "operator",
    "private",
    "protocol",
    "public",
    "rethrows",
    "static",
    "struct",
    "subscript",
    "typealias",
    "var",
    "break",
    "case",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "fallthrough",
    "for",
    "guard",
    "if",
    "in",
    "repeat",
    "return",
    "switch",
    "where",
    "while",
    "as",
    "catch",
    "false",
    "is",
    "nil",
    "super",
    "self",
    "Self",
    "throw",
    "throws",
    "true",
    "try",
    "Any",
    "Foundation",
    "Swift",
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArtifactInput {
    package_name: String,
    namespace_override: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactIdentity {
    /// The backend namespace in the spelling its runner uses. For Rust this
    /// is the rustc crate identifier, after Cargo's `-` → `_` mapping.
    pub(crate) namespace: String,
}

/// Ordered artifact-address options shared by every runner CLI.
///
/// Semantic interface data deliberately has no representation here. A named
/// protocol owns that contract; these arguments identify only the artifact
/// produced by the package build.
#[derive(Default)]
pub(crate) struct ArtifactIdentityArgs {
    artifacts: Vec<ArtifactInput>,
}

impl ArtifactIdentityArgs {
    /// Consume one artifact-identity option from a runner's ordinary argv
    /// parser. Returns `Ok(false)` when `arg` belongs to that runner.
    pub(crate) fn consume<'a, I>(&mut self, arg: &str, iter: &mut I) -> Result<bool, String>
    where
        I: Iterator<Item = &'a String>,
    {
        match arg {
            "--package-name" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--package-name requires a value".to_owned())?;
                self.record_package_name(value)?;
                Ok(true)
            }
            "--artifact-namespace" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--artifact-namespace requires a value".to_owned())?;
                self.record_namespace_override(value)?;
                Ok(true)
            }
            _ if arg.starts_with("--package-name=") => {
                self.record_package_name(&arg["--package-name=".len()..])?;
                Ok(true)
            }
            _ if arg.starts_with("--artifact-namespace=") => {
                self.record_namespace_override(&arg["--artifact-namespace=".len()..])?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn record_package_name(&mut self, value: &str) -> Result<(), String> {
        if !kio_package_name(value) {
            return Err(format!(
                "invalid --package-name `{value}`: each lowercase ASCII word contains \
                 letters followed by optional digits; separate words with one `_`, \
                 with at most one leading `_` and any number of trailing underscores"
            ));
        }
        self.artifacts.push(ArtifactInput {
            package_name: value.to_owned(),
            namespace_override: None,
        });
        Ok(())
    }

    /// Record the effective namespace override for the preceding
    /// `--package-name`. The runner invocation is already backend-local: the
    /// harness selects the configured value before this CLI boundary.
    fn record_namespace_override(&mut self, value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err("--artifact-namespace requires a non-empty namespace".to_owned());
        }
        let artifact = self.artifacts.last_mut().ok_or_else(|| {
            "--artifact-namespace must follow the --package-name of the artifact it describes"
                .to_owned()
        })?;
        if artifact.namespace_override.is_some() {
            return Err(
                "--artifact-namespace specified more than once for the preceding --package-name"
                    .to_owned(),
            );
        }
        artifact.namespace_override = Some(value.to_owned());
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        backend: &str,
        expected_packages: usize,
    ) -> Result<Vec<ArtifactIdentity>, String> {
        if self.artifacts.len() != expected_packages {
            return Err(format!(
                "runner requires {expected_packages} --package-name argument{}, got {}",
                if expected_packages == 1 { "" } else { "s" },
                self.artifacts.len()
            ));
        }
        self.artifacts
            .iter()
            .map(|artifact| {
                resolve(
                    backend,
                    &artifact.package_name,
                    artifact.namespace_override.as_deref(),
                )
                .map(|namespace| ArtifactIdentity { namespace })
            })
            .collect()
    }
}

/// Resolve one artifact's exact runner namespace without reading emitted
/// source. Keep this independent default derivation deliberately small and
/// vector-tested against the public backend contract: importing compiler code
/// here would make the runner agree with an emitter bug by construction.
fn resolve(
    backend: &str,
    package_name: &str,
    explicit_namespace: Option<&str>,
) -> Result<String, String> {
    if !kio_package_name(package_name) {
        return Err(format!(
            "invalid --package-name `{package_name}`: expected a Kio package name"
        ));
    }
    if let Some(namespace) = explicit_namespace {
        if backend == "kio-prime" {
            return Err(
                "kio-prime artifacts carry the source package name verbatim and have no separate backend namespace"
                    .to_owned(),
            );
        }
        validate_explicit_namespace(backend, namespace)?;
        return Ok(if backend == "rust" {
            namespace.replace('-', "_")
        } else {
            namespace.to_owned()
        });
    }
    let namespace = match backend {
        "js" | "ts" => package_name.to_owned(),
        "python" => keyword_escape(package_name, PYTHON_KEYWORDS),
        "java" => {
            if JAVA_KEYWORDS.contains(&package_name)
                || matches!(
                    source_brand(package_name, false).as_str(),
                    "KioRuntime" | "Shapes"
                )
            {
                escape_default(package_name)
            } else {
                package_name.to_owned()
            }
        }
        "rust" => keyword_escape(package_name, RUST_KEYWORDS),
        "go" => {
            if GO_KEYWORDS.contains(&package_name)
                || matches!(package_name, "internal" | "main")
                || matches!(
                    source_brand(package_name, false).as_str(),
                    "KioSum" | "Product" | "Sum" | "Unit"
                )
            {
                escape_default(package_name)
            } else {
                package_name.to_owned()
            }
        }
        "swift" => {
            let base = source_brand(package_name, false);
            if SWIFT_RESERVED.contains(&base.as_str()) || swift_support_brand_collision(&base) {
                source_brand(package_name, true)
            } else {
                base
            }
        }
        "haskell" => {
            let base = source_brand(package_name, false);
            if matches!(base.as_str(), "Main" | "Prelude") {
                source_brand(package_name, true)
            } else {
                base
            }
        }
        "kio-prime" => package_name.to_owned(),
        _ => return Err(format!("unknown runner backend `{backend}`")),
    };
    Ok(namespace)
}

fn ascii_ident(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate the source-language package-name contract independently of the
/// compiler parser. Package names use Kio value-name spelling: lowercase
/// words of lowercase ASCII letters followed by optional digits, separated by
/// one underscore, with at most one leading and any trailing underscores.
fn kio_package_name(value: &str) -> bool {
    if value.starts_with("__") {
        return false;
    }
    value.trim_matches('_').split('_').all(|word| {
        let letters = word.bytes().take_while(u8::is_ascii_lowercase).count();
        letters > 0 && word[letters..].bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Validate an explicit namespace against the backend's published grammar.
///
/// Besides keeping the runner independent of compiler implementation, this is
/// a security boundary: namespace strings become artifact paths, import names,
/// and native compiler arguments. They must never be accepted as arbitrary
/// path fragments merely because the corpus harness supplied them.
fn validate_explicit_namespace(backend: &str, value: &str) -> Result<(), String> {
    let valid = match backend {
        "js" | "ts" => ascii_ident(value),
        "python" => ascii_ident(value) && !PYTHON_KEYWORDS.contains(&value),
        "java" => {
            let segments_ok = value.split('.').all(java_ident);
            let brand = pascal_case(value.rsplit('.').next().unwrap_or(value));
            segments_ok
                && !value
                    .split('.')
                    .any(|segment| JAVA_KEYWORDS.contains(&segment))
                && !value
                    .rsplit('.')
                    .next()
                    .unwrap_or(value)
                    .trim_matches('_')
                    .is_empty()
                && !matches!(brand.as_str(), "KioRuntime" | "Shapes")
        }
        "rust" => {
            let mut chars = value.chars();
            let grammar_ok = chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
            let crate_ident = value.replace('-', "_");
            grammar_ok
                && !RUST_KEYWORDS.contains(&crate_ident.as_str())
                && !crate_ident.trim_matches('_').is_empty()
        }
        "go" => {
            let mut chars = value.chars();
            let grammar_ok = chars
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            grammar_ok
                && !GO_KEYWORDS.contains(&value)
                && !matches!(value, "internal" | "main")
                && !value.trim_matches('_').is_empty()
                && !matches!(
                    pascal_case(value).as_str(),
                    "KioSum" | "Product" | "Sum" | "Unit"
                )
        }
        "swift" => {
            let mut chars = value.chars();
            chars.next().is_some_and(|c| c.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !SWIFT_RESERVED.contains(&value)
                && !swift_support_brand_collision(value)
        }
        "haskell" => {
            !matches!(value, "Main" | "Prelude")
                && value.split('.').all(|segment| {
                    let mut chars = segment.chars();
                    chars.next().is_some_and(|c| c.is_ascii_uppercase())
                        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\''))
                })
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid --artifact-namespace `{value}` for backend `{backend}`"
        ))
    }
}

fn java_ident(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '$'))
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
}

fn keyword_escape(name: &str, keywords: &[&str]) -> String {
    if keywords.contains(&name) {
        escape_default(name)
    } else {
        name.to_owned()
    }
}

pub(crate) fn pascal_case(name: &str) -> String {
    if let Some(encoded) = name.strip_prefix("__kio_pkg_") {
        let decoded = encoded.replace("_u", "_");
        if kio_package_name(&decoded)
            && !decoded.starts_with('_')
            && !decoded.ends_with('_')
            && escape_default(&decoded) == name
        {
            return source_brand(&decoded, true);
        }
    }
    if kio_package_name(name) {
        return source_brand(name, false);
    }
    format!(
        "KioNs_{}",
        name.bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn escape_default(name: &str) -> String {
    format!("__kio_pkg_{}", name.replace('_', "_u"))
}

fn source_brand(name: &str, force_frame: bool) -> String {
    let leading = name.len() - name.trim_start_matches('_').len();
    let trailing = name.len() - name.trim_end_matches('_').len();
    let words = name
        .trim_matches('_')
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<String>();
    let title = format!("{}{words}{}", "_".repeat(leading), "_".repeat(trailing));
    if force_frame || leading > 0 || trailing > 0 {
        format!("KioPkg_{}", title.replace('_', "_u"))
    } else {
        title
    }
}

#[cfg(any(feature = "rust", feature = "python", test))]
#[allow(dead_code)] // Each binary compiles this module with the workspace's feature set.
pub(crate) fn value_brand(namespace: &str) -> String {
    let brand = pascal_case(namespace);
    if brand.starts_with("KioPkg_") || brand.starts_with("KioNs_") {
        brand
    } else {
        brand[..1].to_ascii_lowercase() + &brand[1..]
    }
}

fn swift_support_brand_collision(name: &str) -> bool {
    matches!(name, "KioUnit" | "Product" | "Sum")
        || [
            "KioFacade_",
            "KioHostType_",
            "KioHostTypeMk_",
            "KioNewtype_",
            "KioNewtypeMk_",
            "KioForall_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || name.strip_prefix("KioApply").is_some_and(|arity| {
            !arity.is_empty() && arity.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_namespaces_keep_disjoint_brand_domains() {
        let namespaces = [
            "foo_bar",
            "FooBar",
            "a__b",
            "KioNs_615f5f62",
            "_foo",
            "KioPkg__uFoo",
            "foo_",
            "__kio_pkg_foo_u",
            "__kio_pkg__ufoo",
            "__kio_pkg_foo",
            "KioPkg_Foo",
        ];
        let mut brands = std::collections::BTreeSet::new();
        let mut factories = std::collections::BTreeSet::new();
        for namespace in namespaces {
            for backend in ["js", "python", "java"] {
                validate_explicit_namespace(backend, namespace).unwrap();
            }
            assert!(brands.insert(pascal_case(namespace)), "{namespace}");
            assert!(factories.insert(value_brand(namespace)), "{namespace}");
        }
        assert_eq!(pascal_case("foo_bar"), "FooBar");
        assert_eq!(pascal_case("FooBar"), "KioNs_466f6f426172");
        assert_eq!(pascal_case("__kio_pkg_foo"), "KioPkg_Foo");
        assert!(pascal_case("__kio_pkg_foo_u").starts_with("KioNs_"));
        assert!(pascal_case("__kio_pkg__ufoo").starts_with("KioNs_"));
    }

    #[test]
    fn fixed_default_vectors_match_the_public_backend_contract() {
        let vectors = [
            ("js", "greeter", "greeter"),
            ("ts", "greeter", "greeter"),
            ("python", "match", "match"),
            ("python", "class", "__kio_pkg_class"),
            ("java", "package", "__kio_pkg_package"),
            ("java", "shapes", "__kio_pkg_shapes"),
            ("rust", "two_part", "two_part"),
            ("rust", "match", "__kio_pkg_match"),
            ("go", "select", "__kio_pkg_select"),
            ("go", "main", "__kio_pkg_main"),
            ("go", "unit", "__kio_pkg_unit"),
            ("swift", "csv_reconcile", "CsvReconcile"),
            ("swift", "foundation", "KioPkg_Foundation"),
            ("haskell", "csv_reconcile", "CsvReconcile"),
            ("haskell", "main", "KioPkg_Main"),
        ];
        for (backend, package, expected) in vectors {
            assert_eq!(resolve(backend, package, None).as_deref(), Ok(expected));
        }
    }

    #[test]
    fn exact_override_wins_and_rust_uses_its_crate_identifier() {
        assert_eq!(
            resolve("haskell", "ignored", Some("Com.Acme.Pkg")).as_deref(),
            Ok("Com.Acme.Pkg")
        );
        assert_eq!(
            resolve("rust", "ignored", Some("acme-pkg")).as_deref(),
            Ok("acme_pkg")
        );
    }

    #[test]
    fn kio_prime_has_no_separate_namespace_override() {
        let mut args = ArtifactIdentityArgs::default();
        args.record_package_name("guest").unwrap();
        args.record_namespace_override("other").unwrap();
        assert!(
            args.resolve("kio-prime", 1)
                .unwrap_err()
                .contains("no separate backend namespace")
        );
    }

    #[test]
    fn ordered_descriptors_allow_duplicate_package_names_with_distinct_namespaces() {
        let mut args = ArtifactIdentityArgs::default();
        args.record_package_name("same").unwrap();
        args.record_namespace_override("PkgA").unwrap();
        args.record_package_name("same").unwrap();
        args.record_namespace_override("PkgB").unwrap();

        assert_eq!(
            args.resolve("haskell", 2).unwrap(),
            [
                ArtifactIdentity {
                    namespace: "PkgA".to_owned(),
                },
                ArtifactIdentity {
                    namespace: "PkgB".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn namespace_override_must_follow_its_package_descriptor() {
        let mut args = ArtifactIdentityArgs::default();
        assert!(
            args.record_namespace_override("PkgB")
                .unwrap_err()
                .contains("must follow the --package-name")
        );
    }

    #[test]
    fn one_artifact_cannot_repeat_its_namespace_override() {
        let mut args = ArtifactIdentityArgs::default();
        args.record_package_name("alpha").unwrap();
        args.record_namespace_override("PkgA").unwrap();
        assert!(
            args.record_namespace_override("PkgB")
                .unwrap_err()
                .contains("specified more than once")
        );
    }

    #[test]
    fn package_and_namespace_identity_cannot_escape_artifact_paths() {
        let mut args = ArtifactIdentityArgs::default();
        assert!(args.record_package_name("../guest").is_err());
        args.record_package_name("guest").unwrap();
        args.record_namespace_override("../../payload").unwrap();
        assert!(args.resolve("js", 1).is_err());

        let mut args = ArtifactIdentityArgs::default();
        args.record_package_name("guest").unwrap();
        args.record_namespace_override("class").unwrap();
        assert!(args.resolve("python", 1).is_err());
    }

    #[test]
    fn package_names_follow_the_kio_value_name_contract() {
        for accepted in ["a", "alpha", "a1", "_a", "a1_b2", "two_part", "foo__"] {
            assert!(kio_package_name(accepted), "rejected `{accepted}`");
        }
        for rejected in [
            "",
            "_",
            "_1",
            "_1a",
            "a1b",
            "foo_123",
            "foo__bar",
            "__internal",
            "Upper",
            "two-part",
            "with/slash",
            "é",
        ] {
            assert!(!kio_package_name(rejected), "accepted `{rejected}`");
        }
    }

    #[test]
    fn explicit_namespace_grammar_is_backend_specific() {
        for (backend, accepted) in [
            ("js", "pkg_name"),
            ("python", "pkg_name"),
            ("java", "com.acme.pkg"),
            ("rust", "acme-pkg"),
            ("go", "acme_pkg"),
            ("swift", "AcmePkg"),
            ("haskell", "Com.Acme.Pkg"),
        ] {
            validate_explicit_namespace(backend, accepted)
                .unwrap_or_else(|e| panic!("{backend} rejected {accepted}: {e}"));
        }
        for (backend, rejected) in [
            ("js", "pkg/name"),
            ("python", "class"),
            ("java", "com.class.pkg"),
            ("rust", "match"),
            ("go", "main"),
            ("swift", "Foundation"),
            ("haskell", "Main"),
        ] {
            assert!(
                validate_explicit_namespace(backend, rejected).is_err(),
                "{backend} accepted {rejected}"
            );
        }
    }
}

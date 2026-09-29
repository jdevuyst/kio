//! Package namespaces and their deterministic public facade brands.
//!
//! Source-derived artifact/import names follow each host's module convention.
//! Public brands preserve source word boundaries and underscore affixes.
//! Fixed host/support conflicts use reserved exact frames, independently of
//! declaration occupancy. Explicit overrides retain their host validators.

use super::public_names::{encode_source_identity, host_name_core};

const DEFAULT_ESCAPE: &str = "__kio_pkg_";

fn escaped_default_namespace(source: &str) -> String {
    format!("{DEFAULT_ESCAPE}{}", encode_source_identity(source))
}

fn source_brand(source: &str, force_frame: bool) -> String {
    let mut title = host_name_core(source);
    if let Some(index) = title.bytes().position(|byte| byte.is_ascii_alphabetic()) {
        title[index..index + 1].make_ascii_uppercase();
    }
    if force_frame || source.starts_with('_') || source.ends_with('_') {
        format!("KioPkg_{}", encode_source_identity(&title))
    } else {
        title
    }
}

fn escaped_default_source(namespace: &str) -> Option<String> {
    let encoded = namespace.strip_prefix(DEFAULT_ESCAPE)?;
    let mut source = String::new();
    let mut chars = encoded.chars();
    while let Some(ch) = chars.next() {
        if ch == '_' {
            if chars.next()? != 'u' {
                return None;
            }
            source.push('_');
        } else {
            source.push(ch);
        }
    }
    // Only unmarked defaults need keyword/support escaping. Keeping this
    // domain unmarked separates its forced frame from affixed source brands.
    (crate::naming::is_value_name(&source) && !source.starts_with('_') && !source.ends_with('_'))
        .then_some(source)
}

/// Rust reserved words (strict + reserved, 2024 edition, plus the
/// weak keywords usable in ident position that `use` paths reject).
/// A crate name colliding with one is unusable from host code —
/// `use <crate>::…` rejects keyword idents — so the default
/// derivation mangles them and explicit values reject them.
const RUST_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Java reserved words (keywords + literals + contextual `var` /
/// `record`-safe set). A package-declaration segment or generated
/// identifier colliding with one is unusable from host code, so the
/// default derivation mangles and explicit values reject them.
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

/// Whether `segment` is a legal Java identifier (ASCII ident grammar —
/// the package name grammar is ASCII, so the emitted namespace stays
/// ASCII) that is not a Java reserved word.
fn java_ident_grammar_ok(segment: &str) -> bool {
    let mut chars = segment.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    first_ok && rest_ok
}

/// These file-named support classes share the handle's Java package.
/// Their identities are reserved independently of the package's declarations.
const JAVA_SUPPORT_BRANDS: &[&str] = &["KioRuntime", "Shapes"];

pub fn default_java_namespace(pkg_name: &str) -> String {
    if JAVA_KEYWORDS.contains(&pkg_name)
        || JAVA_SUPPORT_BRANDS.contains(&source_brand(pkg_name, false).as_str())
    {
        escaped_default_namespace(pkg_name)
    } else {
        pkg_name.to_owned()
    }
}

/// Validate an explicit java `namespace` value: a dotted chain of Java
/// identifiers (`com.acme.greeter`), no segment a reserved word.
pub fn validate_java_namespace(value: &str) -> Result<(), String> {
    if value.is_empty() || !value.split('.').all(java_ident_grammar_ok) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `java` backend: \
             a Java package name is a `.`-separated chain of Java \
             identifiers"
        ));
    }
    if let Some(kw) = value.split('.').find(|s| JAVA_KEYWORDS.contains(s)) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `java` backend: \
             segment `{kw}` is a Java reserved word, so the `package` \
             declaration would not compile"
        ));
    }
    let brand = pascal_case(value.rsplit('.').next().unwrap_or(value));
    if value
        .rsplit('.')
        .next()
        .unwrap_or(value)
        .trim_matches('_')
        .is_empty()
    {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `java` backend: \
             no branded facade name can be derived from its final segment"
        ));
    }
    if JAVA_SUPPORT_BRANDS.contains(&brand.as_str()) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `java` backend: \
             its handle `{brand}` collides with the emitted support class \
             `{brand}.java`"
        ));
    }
    Ok(())
}

/// Source spelling is the crate namespace; fixed keywords use the exact
/// default-escape class outside the user package-name image.
pub fn default_rust_namespace(pkg_name: &str) -> String {
    if RUST_KEYWORDS.contains(&pkg_name) {
        escaped_default_namespace(pkg_name)
    } else {
        pkg_name.to_owned()
    }
}

/// Derive a public brand from the effective namespace alone. Source-shaped
/// names use readable word casing and exact affix frames; escaped defaults
/// reconstruct their unmarked source component. Every other host override
/// uses an exact byte frame, disjoint from both source-derived brand classes.
/// Uppercase-module hosts use their effective namespace directly instead.
pub fn pascal_case(name: &str) -> String {
    if let Some(source) = escaped_default_source(name) {
        return source_brand(&source, true);
    }
    if crate::naming::is_value_name(name) {
        return source_brand(name, false);
    }
    let mut brand = String::from("KioNs_");
    for byte in name.bytes() {
        use std::fmt::Write;
        write!(&mut brand, "{byte:02x}").expect("writing a string cannot fail");
    }
    brand
}

/// Public package component inside a value-level factory's fixed frame.
pub fn value_brand(namespace: &str) -> String {
    let mut brand = pascal_case(namespace);
    if !brand.starts_with("KioPkg_") && !brand.starts_with("KioNs_") {
        brand[..1].make_ascii_lowercase();
    }
    brand
}

/// The 25 Go keywords. A Go package named after one is unusable from
/// host code (the qualifier position rejects keywords), so the default
/// derivation mangles them and explicit values reject them.
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

/// Go package names a host cannot import: `main` is the program
/// package (Go rejects `import "main"`), and `internal` is
/// import-restricted as a path element. A default deriving one would
/// emit an artifact no host could load, so they mangle exactly like
/// keywords.
const GO_UNIMPORTABLE: &[&str] = &["internal", "main"];

/// Branded handle names colliding with the Go backend's fixed package types.
///
/// `Unit` is runtime support. `Product` and `Sum` are the canonical anonymous
/// binary facade shells, and `KioSum` is the shared concrete sum carrier. A
/// package must reserve all four even when its current boundary does not use a
/// structural shell: adding an unrelated declaration later must not introduce
/// a handle/type collision. The default derivation mangles these names;
/// explicit values reject them.
const GO_SUPPORT_BRANDS: &[&str] = &["KioSum", "Product", "Sum", "Unit"];

/// Default Go package clauses retain source spelling unless a fixed host or
/// support identity requires the disjoint default-escape class.
pub fn default_go_namespace(pkg_name: &str) -> String {
    if GO_KEYWORDS.contains(&pkg_name)
        || GO_UNIMPORTABLE.contains(&pkg_name)
        || GO_SUPPORT_BRANDS.contains(&source_brand(pkg_name, false).as_str())
    {
        escaped_default_namespace(pkg_name)
    } else {
        pkg_name.to_owned()
    }
}

/// Validate an explicit Go `namespace` value: a Go package name host
/// code spells as the import qualifier. Accepts `[a-z_][a-z0-9_]*`
/// (lower-case, per Go convention, keeping the derived handle names' case
/// split deterministic), with no keyword, import, or fixed-brand collision.
pub fn validate_go_namespace(value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !(first_ok && rest_ok) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `go` backend: \
             a Go package name matches `[a-z_][a-z0-9_]*`"
        ));
    }
    if GO_KEYWORDS.contains(&value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `go` backend: \
             `{value}` is a Go keyword, so host code could not qualify the package"
        ));
    }
    if GO_UNIMPORTABLE.contains(&value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `go` backend: \
             a Go host cannot import a package named `{value}`"
        ));
    }
    let brand = pascal_case(value);
    if value.trim_matches('_').is_empty() {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `go` backend: \
             no branded facade name can be derived from it"
        ));
    }
    if GO_SUPPORT_BRANDS.contains(&brand.as_str()) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `go` backend: \
             its handle `{brand}` collides with the compiler-generated package \
             type `{brand}`"
        ));
    }
    Ok(())
}

/// Validate an explicit rust `namespace` value: a Cargo crate name
/// host code can also spell in `use` paths after Cargo's `-` → `_`
/// mapping. Accepts `[A-Za-z_][A-Za-z0-9_-]*` with no Rust-keyword
/// collision; rejects everything else with the grammar named.
pub fn validate_rust_namespace(value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !(first_ok && rest_ok) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `rust` backend: \
             a crate name matches `[A-Za-z_][A-Za-z0-9_-]*`"
        ));
    }
    let crate_ident = value.replace('-', "_");
    if RUST_KEYWORDS.contains(&crate_ident.as_str()) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `rust` backend: \
             `{crate_ident}` is a Rust keyword, so host code could not `use` the crate"
        ));
    }
    if crate_ident.trim_matches('_').is_empty() {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `rust` backend: \
             no branded facade name can be derived from it"
        ));
    }
    Ok(())
}

/// Python's 35 hard keywords (`keyword.kwlist`, Python 3.10+). A module
/// whose stem is one of these cannot be brought in with an `import
/// <stem>` statement — `import <kw>` is a syntax error — so the default
/// derivation mangles them and explicit values reject them. Python's
/// *soft* keywords (`match`, `case`, `type`, `_`) are deliberately absent:
/// each is a legal identifier outside its statement context, so `import
/// match` parses and a stem spelling one needs no mangle.
const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// Whether `name` is one of Python's hard keywords — a name that cannot
/// be written as a Python attribute, method, or class-member spelling
/// (`def and(...)` / `x.and` are syntax errors). Soft keywords (`match`,
/// `case`, `type`, `_`) are legal identifiers outside their statement
/// context and return `false`. The `.pyi` stub renderer uses this to
/// route such a member through `getattr`-typed access rather than a dot
/// spelling (`specs/backends/python.md` § Item naming).
pub(crate) fn is_python_keyword(name: &str) -> bool {
    PYTHON_KEYWORDS.contains(&name)
}

/// Python module stems retain source spelling; hard keywords use the exact
/// default-escape class so the emitted module remains importable.
///
/// A stem that shadows a standard-library module (a package named `json`,
/// say) is **not** mangled. Python import resolution is the host's to
/// control — `sys.path` order decides which `json` wins, and a host that
/// loads the artifact by file path (as the test runner does) sidesteps
/// the module namespace entirely — so a stdlib-name collision is
/// resolvable rather than fatal. Mangling it would corrupt the branded
/// name for a case the host already governs; `specs/backends/python.md`
/// § Output layout documents the resolution as the host's responsibility.
pub fn default_python_namespace(pkg_name: &str) -> String {
    if PYTHON_KEYWORDS.contains(&pkg_name) {
        escaped_default_namespace(pkg_name)
    } else {
        pkg_name.to_owned()
    }
}

/// Explicit Python module stems use the host's ASCII identifier grammar and
/// exclude hard keywords. The public factory component uses `value_brand`.
pub fn validate_python_namespace(value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !(first_ok && rest_ok) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `python` backend: \
             a module stem matches `[A-Za-z_][A-Za-z0-9_]*`"
        ));
    }
    if PYTHON_KEYWORDS.contains(&value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `python` backend: \
             `{value}` is a Python keyword, so host code could not `import` the module"
        ));
    }
    Ok(())
}

/// Default namespace for the js and ts backends: the kio package name is
/// the artifact stem verbatim (`<ns>.js` / `<ns>.d.ts`), and that stem
/// doubles as the brand root. A package name is an `IDENT`
/// (`[A-Za-z_][A-Za-z0-9_]*` — `specs/grammar.md`), so it always starts
/// an identifier and is therefore always brandable; nothing is mangled.
///
/// No reserved-word mangling is needed (unlike Go / Java): the brand is
/// the *PascalCased* stem, so the handle (`Greeter`) and host type
/// (`GreeterHost`) capitalize their initial and can never spell a
/// (lower-case) JS reserved word, and the factory is `create`-prefixed
/// (`createGreeter`). The stem itself only ever occupies a filename and
/// object-property positions, where JS admits reserved words.
///
/// One `js` pair serves **both** the `js` and `ts` build arms: the
/// `.d.ts` skin types the same byte-identical `.js`, so both derive the
/// stem and brand from it identically.
pub fn default_js_namespace(pkg_name: &str) -> String {
    pkg_name.to_owned()
}

/// Validate an explicit js / ts `namespace` value: a portable module
/// stem `[A-Za-z_][A-Za-z0-9_]*` — the `<ns>.js` / `<ns>.d.ts` artifact
/// stem, which must also be brandable (PascalCased into the handle
/// `<Handle>`, the host type `<Handle>Host`, and the factory
/// `create<Handle>`). A leading identifier char is guaranteed by the
/// grammar, so every accepted value is well-formed as a brand root.
/// Shared by the `js` and `ts` arms — the two artifacts share a stem.
pub fn validate_js_namespace(value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if first_ok && rest_ok {
        return Ok(());
    }
    Err(format!(
        "invalid `namespace` value `{value}` for the `js`/`ts` backend: \
         a module stem matches `[A-Za-z_][A-Za-z0-9_]*`"
    ))
}

/// Swift reserved words. A module name or a generated identifier (the
/// PascalCase package-handle type) colliding with one is unusable from
/// host code, so the default derivation mangles them and explicit values
/// reject them. The default PascalCases the package name, so the members
/// that actually collide with a default are the capital-initial reserved
/// words (`Any`, `Self`); the rest guard the explicit path and mirror the
/// sibling backends' full keyword lists.
const SWIFT_KEYWORDS: &[&str] = &[
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
];

/// Swift stdlib / framework module names whose shadowing breaks
/// compilation. `Swift` is rejected by swiftc outright (`module name
/// "Swift" is reserved for the standard library`) and is implicitly
/// imported by every emitted file; `Foundation` link-collides for any
/// host that also imports the real Foundation (the test runner's driver
/// does, and most hosts do). Either is legal-looking but unloadable —
/// the Swift analogue of Go's `main` / `internal` — so the default
/// derivation mangles them and explicit values reject them.
const SWIFT_STDLIB_MODULES: &[&str] = &["Foundation", "Swift"];

/// Whether a PascalCase Swift name collides with a reserved word or a
/// shadowing stdlib module — the two classes the default derivation
/// mangles and explicit `namespace` values reject.
fn swift_name_reserved(name: &str) -> bool {
    SWIFT_KEYWORDS.contains(&name) || SWIFT_STDLIB_MODULES.contains(&name)
}

/// Branded handle names colliding with the Swift backend's top-level support
/// declarations. The exact fixed names and generated-name grammars are
/// reserved so the handle never redeclares a shape, nominal carrier, abstract
/// application, or rank-N witness in the artifact module.
const SWIFT_SUPPORT_BRANDS: &[&str] = &[
    "KioUnit",
    "Product",
    "Sum",
    "KioNative",
    "KioNativeConstructor",
];
const SWIFT_SUPPORT_BRAND_PREFIXES: &[&str] = &[
    "KioFacade_",
    "KioHostType_",
    "KioHostTypeMk_",
    "KioHostTypeIdentity_",
    "KioNewtype_",
    "KioNewtypeMk_",
    "KioForall_",
];

fn swift_support_brand_collision(value: &str) -> bool {
    SWIFT_SUPPORT_BRANDS.contains(&value)
        || SWIFT_SUPPORT_BRAND_PREFIXES
            .iter()
            .any(|prefix| value.starts_with(prefix))
        || value.strip_prefix("KioApply").is_some_and(|arity| {
            !arity.is_empty() && arity.bytes().all(|byte| byte.is_ascii_digit())
        })
}

pub fn default_swift_namespace(pkg_name: &str) -> String {
    let base = source_brand(pkg_name, false);
    if swift_name_reserved(&base) || swift_support_brand_collision(&base) {
        source_brand(pkg_name, true)
    } else {
        base
    }
}

/// Validate an explicit swift `namespace` value: a Swift module name the
/// host writes in `import <value>`. Requires a **capital initial** (what
/// the default PascalCase derivation produces — Swift's UpperCamelCase
/// module convention), then `[A-Za-z0-9_]*`; rejects reserved words and
/// shadowing stdlib modules. Requiring the capital initial keeps the
/// branded handle's case split deterministic and the module==handle shape
/// consistent with the default.
pub fn validate_swift_namespace(value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_uppercase());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !(first_ok && rest_ok) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `swift` backend: \
             a Swift module name matches `[A-Z][A-Za-z0-9_]*` (a capital \
             initial, matching the default derivation)"
        ));
    }
    if SWIFT_KEYWORDS.contains(&value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `swift` backend: \
             `{value}` is a Swift reserved word, so host code could not `import` it"
        ));
    }
    if SWIFT_STDLIB_MODULES.contains(&value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `swift` backend: \
             `{value}` shadows a Swift stdlib module the artifact or its host imports"
        ));
    }
    if swift_support_brand_collision(value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `swift` backend: \
             it collides with a generated Swift support type named `{value}`"
        ));
    }
    Ok(())
}

/// No keyword table is needed here (unlike the other backends): every
/// Haskell keyword (`case`, `class`, `data`, `where`, …) is lowercase,
/// and a GHC module name — like the derived namespace — always begins
/// with an uppercase letter, so a keyword collision is impossible.
const HASKELL_RESERVED_MODULES: &[&str] = &["Main", "Prelude"];

/// Haskell module names use the public brand, including the exact package
/// frame for affixes and fixed `Main`/`Prelude` conflicts.
pub fn default_haskell_namespace(pkg_name: &str) -> String {
    let base = source_brand(pkg_name, false);
    if HASKELL_RESERVED_MODULES.contains(&base.as_str()) {
        source_brand(pkg_name, true)
    } else {
        base
    }
}

/// Whether `segment` is a legal GHC module-name segment:
/// `[A-Z][A-Za-z0-9_']*` (upper-case initial, then module-ident chars —
/// the `'` prime is legal in Haskell identifiers).
fn haskell_module_segment_ok(segment: &str) -> bool {
    let mut chars = segment.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_uppercase());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'');
    first_ok && rest_ok
}

/// Validate an explicit haskell `namespace` value: a dotted GHC module
/// path (`Com.Acme.Greeter`), each segment `[A-Z][A-Za-z0-9_']*`. The
/// whole value may not be `Main` or `Prelude`
/// ([`HASKELL_RESERVED_MODULES`]) — but a dotted path whose *final*
/// segment is one of those is fine (`Acme.Main` is its own module,
/// colliding with neither the program `Main` nor the implicit `Prelude`).
pub fn validate_haskell_namespace(value: &str) -> Result<(), String> {
    if value.is_empty() || !value.split('.').all(haskell_module_segment_ok) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `haskell` backend: \
             a GHC module name is a `.`-separated chain of segments each \
             matching `[A-Z][A-Za-z0-9_']*`"
        ));
    }
    if HASKELL_RESERVED_MODULES.contains(&value) {
        return Err(format!(
            "invalid `namespace` value `{value}` for the `haskell` backend: \
             a module named `{value}` collides with the host's own `{value}` \
             (the program's `Main`, or the implicit `Prelude`)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_names_and_brands_preserve_affixes_and_fixed_conflicts() {
        let sources = [
            "foo",
            "_foo",
            "foo_",
            "foo__",
            "foo_bar",
            "foo123_bar4",
            "kio_pkg_foo",
            "class",
            "class_",
            "main",
            "main_",
            "swift",
            "swift_pkg",
            "shapes",
            "shapes_pkg",
            "kio_runtime",
            "kio_runtime_pkg",
            "product",
            "product_pkg",
        ];
        type NamespaceRule = (
            &'static str,
            fn(&str) -> String,
            fn(&str) -> Result<(), String>,
        );
        let rules: [NamespaceRule; 7] = [
            ("js/ts", default_js_namespace, validate_js_namespace),
            (
                "python",
                default_python_namespace,
                validate_python_namespace,
            ),
            ("java", default_java_namespace, validate_java_namespace),
            ("rust", default_rust_namespace, validate_rust_namespace),
            ("go", default_go_namespace, validate_go_namespace),
            ("swift", default_swift_namespace, validate_swift_namespace),
            (
                "haskell",
                default_haskell_namespace,
                validate_haskell_namespace,
            ),
        ];
        for (host, default, validate) in rules {
            let mut namespaces = std::collections::BTreeSet::new();
            let mut brands = std::collections::BTreeSet::new();
            for source in sources {
                let namespace = default(source);
                assert!(
                    validate(&namespace).is_ok(),
                    "{host}: {source} -> {namespace}"
                );
                let brand = if matches!(host, "swift" | "haskell") {
                    namespace.clone()
                } else {
                    pascal_case(&namespace)
                };
                assert!(brands.insert(brand), "{host}: brand for {source}");
                assert!(
                    namespaces.insert(namespace),
                    "{host}: namespace for {source}"
                );
            }
        }
        for (source, brand) in [
            ("foo_bar", "FooBar"),
            ("_foo_bar", "KioPkg__uFooBar"),
            ("foo_bar_", "KioPkg_FooBar_u"),
            ("foo_bar__", "KioPkg_FooBar_u_u"),
            ("__kio_pkg_class", "KioPkg_Class"),
        ] {
            assert_eq!(pascal_case(source), brand);
        }
        assert_eq!(value_brand("foo_bar"), "fooBar");
        assert_eq!(value_brand("_foo_bar"), "KioPkg__uFooBar");
    }

    #[test]
    fn rust_default_is_the_package_name() {
        assert_eq!(default_rust_namespace("greeter"), "greeter");
        assert_eq!(default_rust_namespace("csv_reconcile"), "csv_reconcile");
    }

    #[test]
    fn rust_default_preserves_source_words() {
        assert_eq!(default_rust_namespace("foo123_bar4"), "foo123_bar4");
    }

    #[test]
    fn rust_default_mangles_keyword_collisions() {
        assert_eq!(default_rust_namespace("match"), "__kio_pkg_match");
        assert_eq!(default_rust_namespace("crate"), "__kio_pkg_crate");
    }

    #[test]
    fn rust_explicit_value_accepts_crate_name_grammar() {
        assert!(validate_rust_namespace("greeter").is_ok());
        assert!(validate_rust_namespace("my-kiopkg").is_ok());
        assert!(validate_rust_namespace("_private").is_ok());
        assert!(validate_rust_namespace("v2_reader").is_ok());
    }

    #[test]
    fn rust_explicit_value_rejects_bad_grammar_and_keywords() {
        assert!(validate_rust_namespace("").is_err());
        assert!(validate_rust_namespace("9lives").is_err());
        assert!(validate_rust_namespace("has space").is_err());
        assert!(validate_rust_namespace("a.b").is_err());
        assert!(validate_rust_namespace("match").is_err());
    }

    #[test]
    fn namespace_brands_preserve_word_and_override_identity() {
        assert_eq!(pascal_case("greeter"), "Greeter");
        assert_eq!(pascal_case("csv_reconcile"), "CsvReconcile");
        assert_eq!(pascal_case("two-part"), "KioNs_74776f2d70617274");
        assert_eq!(pascal_case("a__b"), "KioNs_615f5f62");
    }

    #[test]
    fn explicit_namespaces_cannot_impersonate_another_brand_domain() {
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
            assert!(validate_js_namespace(namespace).is_ok());
            assert!(validate_python_namespace(namespace).is_ok());
            assert!(validate_java_namespace(namespace).is_ok());
            assert!(brands.insert(pascal_case(namespace)), "{namespace}");
            assert!(factories.insert(value_brand(namespace)), "{namespace}");
        }
        assert_eq!(pascal_case("FooBar"), "KioNs_466f6f426172");
        assert_eq!(pascal_case("__kio_pkg_foo"), "KioPkg_Foo");
        assert!(pascal_case("__kio_pkg_foo_u").starts_with("KioNs_"));
        assert!(pascal_case("__kio_pkg__ufoo").starts_with("KioNs_"));
    }

    #[test]
    fn go_default_is_the_package_name() {
        assert_eq!(default_go_namespace("greeter"), "greeter");
        assert_eq!(default_go_namespace("csv_reconcile"), "csv_reconcile");
    }

    #[test]
    fn go_default_mangles_keyword_collisions() {
        assert_eq!(default_go_namespace("select"), "__kio_pkg_select");
        assert_eq!(default_go_namespace("type"), "__kio_pkg_type");
    }

    #[test]
    fn go_default_mangles_unimportable_package_names() {
        assert_eq!(default_go_namespace("main"), "__kio_pkg_main");
        assert_eq!(default_go_namespace("internal"), "__kio_pkg_internal");
    }

    #[test]
    fn go_explicit_value_accepts_package_name_grammar() {
        assert!(validate_go_namespace("greeter").is_ok());
        assert!(validate_go_namespace("acme_greeter").is_ok());
        assert!(validate_go_namespace("v2").is_ok());
        assert!(validate_go_namespace("_x").is_ok());
    }

    #[test]
    fn go_explicit_value_rejects_bad_grammar_and_keywords() {
        assert!(validate_go_namespace("").is_err());
        assert!(validate_go_namespace("9lives").is_err());
        assert!(validate_go_namespace("MixedCase").is_err());
        assert!(validate_go_namespace("has-dash").is_err());
        assert!(validate_go_namespace("select").is_err());
    }

    #[test]
    fn go_explicit_value_rejects_unimportable_package_names() {
        assert!(validate_go_namespace("main").is_err());
        assert!(validate_go_namespace("internal").is_err());
    }

    #[test]
    fn java_default_is_the_package_name() {
        assert_eq!(default_java_namespace("greeter"), "greeter");
    }

    #[test]
    fn java_default_mangles_keyword_collisions() {
        assert_eq!(default_java_namespace("package"), "__kio_pkg_package");
        assert_eq!(default_java_namespace("import"), "__kio_pkg_import");
    }

    #[test]
    fn java_explicit_value_accepts_dotted_packages() {
        assert!(validate_java_namespace("greeter").is_ok());
        assert!(validate_java_namespace("com.acme.greeter").is_ok());
        assert!(validate_java_namespace("_x.y1").is_ok());
    }

    #[test]
    fn java_explicit_value_rejects_bad_segments() {
        assert!(validate_java_namespace("").is_err());
        assert!(validate_java_namespace("a..b").is_err());
        assert!(validate_java_namespace("com.import.x").is_err());
        assert!(validate_java_namespace("9lives").is_err());
        assert!(validate_java_namespace("a-b").is_err());
    }

    #[test]
    fn python_default_is_the_package_name() {
        assert_eq!(default_python_namespace("greeter"), "greeter");
        assert_eq!(default_python_namespace("csv_reconcile"), "csv_reconcile");
    }

    #[test]
    fn python_default_leaves_soft_keywords_and_stdlib_names_alone() {
        // Soft keywords import fine, and a stdlib shadow is the host's to
        // resolve — neither is mangled.
        assert_eq!(default_python_namespace("match"), "match");
        assert_eq!(default_python_namespace("main"), "main");
        assert_eq!(default_python_namespace("json"), "json");
    }

    #[test]
    fn python_default_mangles_keyword_collisions() {
        assert_eq!(default_python_namespace("import"), "__kio_pkg_import");
        assert_eq!(default_python_namespace("class"), "__kio_pkg_class");
    }

    #[test]
    fn python_explicit_value_accepts_identifier_grammar() {
        assert!(validate_python_namespace("greeter").is_ok());
        assert!(validate_python_namespace("acme_greeter").is_ok());
        assert!(validate_python_namespace("v2").is_ok());
        assert!(validate_python_namespace("_x").is_ok());
        assert!(validate_python_namespace("MixedCase").is_ok());
        assert!(validate_python_namespace("match").is_ok());
    }

    #[test]
    fn python_explicit_value_rejects_bad_grammar_and_keywords() {
        assert!(validate_python_namespace("").is_err());
        assert!(validate_python_namespace("9lives").is_err());
        assert!(validate_python_namespace("has-dash").is_err());
        assert!(validate_python_namespace("a.b").is_err());
        assert!(validate_python_namespace("import").is_err());
        assert!(validate_python_namespace("None").is_err());
    }

    #[test]
    fn js_default_is_the_package_name_verbatim() {
        assert_eq!(default_js_namespace("greeter"), "greeter");
        assert_eq!(default_js_namespace("csv_reconcile"), "csv_reconcile");
        // No reserved-word mangling: the PascalCased brand can't collide
        // with a JS reserved word, and the stem is only ever a filename /
        // property key.
        assert_eq!(default_js_namespace("class"), "class");
    }

    #[test]
    fn js_explicit_value_accepts_module_stem_grammar() {
        assert!(validate_js_namespace("greeter").is_ok());
        assert!(validate_js_namespace("acme_greeter").is_ok());
        assert!(validate_js_namespace("v2").is_ok());
        assert!(validate_js_namespace("_x").is_ok());
        // The stem grammar admits any case (unlike Go's lower-case rule).
        assert!(validate_js_namespace("Greeter").is_ok());
    }

    #[test]
    fn js_explicit_value_rejects_bad_grammar() {
        assert!(validate_js_namespace("").is_err());
        assert!(validate_js_namespace("9lives").is_err());
        assert!(validate_js_namespace("has space").is_err());
        assert!(validate_js_namespace("a.b").is_err());
        assert!(validate_js_namespace("has-dash").is_err());
    }

    #[test]
    fn js_brand_is_pascal_case_of_the_stem() {
        // The stem doubles as the brand root: handle `<Handle>`, host
        // `<Handle>Host`, factory `create<Handle>` all derive from this.
        assert_eq!(pascal_case("greeter"), "Greeter");
        assert_eq!(pascal_case("csv_reconcile"), "CsvReconcile");
    }

    #[test]
    fn swift_default_pascal_cases_the_package_name() {
        assert_eq!(default_swift_namespace("greeter"), "Greeter");
        assert_eq!(default_swift_namespace("csv_reconcile"), "CsvReconcile");
        assert_eq!(default_swift_namespace("foo123_bar4"), "Foo123Bar4");
        // The golden package named `main` — `main` is not reserved in
        // Swift (no stdlib `Main`), so it derives cleanly, unlike Go.
        assert_eq!(default_swift_namespace("main"), "Main");
    }

    #[test]
    fn swift_default_mangles_reserved_word_collisions() {
        // The default PascalCases, so the reserved words it can hit are the
        // capital-initial ones: `any` → `Any`, `self` → `Self`.
        assert_eq!(default_swift_namespace("any"), "KioPkg_Any");
        assert_eq!(default_swift_namespace("self"), "KioPkg_Self");
    }

    #[test]
    fn swift_default_mangles_stdlib_module_shadowing() {
        // `Swift` is reserved by swiftc; `Foundation` link-collides for a
        // host that also imports the real Foundation.
        assert_eq!(default_swift_namespace("swift"), "KioPkg_Swift");
        assert_eq!(default_swift_namespace("foundation"), "KioPkg_Foundation");
    }

    #[test]
    fn swift_explicit_value_accepts_upper_camel_module_names() {
        assert!(validate_swift_namespace("Greeter").is_ok());
        assert!(validate_swift_namespace("AcmeGreeter").is_ok());
        assert!(validate_swift_namespace("V2Reader").is_ok());
        assert!(validate_swift_namespace("My_Module").is_ok());
    }

    #[test]
    fn swift_explicit_value_rejects_bad_grammar_and_reserved() {
        assert!(validate_swift_namespace("").is_err());
        // Lowercase initial: rejected per the capital-initial requirement.
        assert!(validate_swift_namespace("greeter").is_err());
        assert!(validate_swift_namespace("9lives").is_err());
        assert!(validate_swift_namespace("Has Space").is_err());
        assert!(validate_swift_namespace("Dotted.Name").is_err());
        assert!(validate_swift_namespace("Has-Dash").is_err());
        assert!(validate_swift_namespace("Any").is_err());
        assert!(validate_swift_namespace("Self").is_err());
    }

    #[test]
    fn swift_explicit_value_rejects_stdlib_module_shadowing() {
        assert!(validate_swift_namespace("Swift").is_err());
        assert!(validate_swift_namespace("Foundation").is_err());
    }

    #[test]
    fn haskell_default_pascal_cases_the_package_name() {
        assert_eq!(default_haskell_namespace("greeter"), "Greeter");
        assert_eq!(default_haskell_namespace("csv_reconcile"), "CsvReconcile");
        assert_eq!(default_haskell_namespace("foo123_bar4"), "Foo123Bar4");
    }

    #[test]
    fn haskell_default_mangles_main_and_prelude() {
        assert_eq!(default_haskell_namespace("main"), "KioPkg_Main");
        assert_eq!(default_haskell_namespace("prelude"), "KioPkg_Prelude");
    }

    #[test]
    fn haskell_explicit_value_accepts_dotted_module_paths() {
        assert!(validate_haskell_namespace("Greeter").is_ok());
        assert!(validate_haskell_namespace("Com.Acme.Greeter").is_ok());
        assert!(validate_haskell_namespace("Greeter'").is_ok());
        assert!(validate_haskell_namespace("V2.Reader_").is_ok());
        // A reserved word as a *non-whole* final segment is its own
        // module, colliding with neither `Main` nor `Prelude`.
        assert!(validate_haskell_namespace("Acme.Main").is_ok());
        assert!(validate_haskell_namespace("Acme.Prelude").is_ok());
    }

    #[test]
    fn haskell_explicit_value_rejects_bad_segments_and_reserved_whole_names() {
        assert!(validate_haskell_namespace("").is_err());
        assert!(validate_haskell_namespace("greeter").is_err()); // lowercase initial
        assert!(validate_haskell_namespace("Com..Greeter").is_err());
        assert!(validate_haskell_namespace("Com.Acme-X").is_err());
        assert!(validate_haskell_namespace("9Lives").is_err());
        assert!(validate_haskell_namespace("Main").is_err());
        assert!(validate_haskell_namespace("Prelude").is_err());
    }

    #[test]
    fn java_default_mangles_support_brand_collisions() {
        assert_eq!(default_java_namespace("shapes"), "__kio_pkg_shapes");
        assert_eq!(
            default_java_namespace("kio_runtime"),
            "__kio_pkg_kio_uruntime"
        );
    }

    #[test]
    fn java_explicit_value_rejects_support_brands_and_empty_brand() {
        assert!(validate_java_namespace("shapes").is_err());
        assert!(validate_java_namespace("com.acme.kio_runtime").is_err());
        assert!(validate_java_namespace("__").is_err());
        assert!(
            validate_java_namespace("com._.x").is_err(),
            "`_` is a Java 9+ reserved word"
        );
        assert!(validate_java_namespace("com.acme.shaping").is_ok());
    }

    #[test]
    fn go_default_mangles_support_brand_collisions() {
        assert_eq!(default_go_namespace("unit"), "__kio_pkg_unit");
        assert_eq!(default_go_namespace("product"), "__kio_pkg_product");
        assert_eq!(default_go_namespace("sum"), "__kio_pkg_sum");
        assert_eq!(default_go_namespace("kio_sum"), "__kio_pkg_kio_usum");
    }

    #[test]
    fn go_explicit_value_rejects_support_brands_and_empty_brand() {
        assert!(validate_go_namespace("unit").is_err());
        assert!(validate_go_namespace("product").is_err());
        assert!(validate_go_namespace("sum").is_err());
        assert!(validate_go_namespace("kio_sum").is_err());
        assert!(validate_go_namespace("____").is_err());
        assert!(validate_go_namespace("united").is_ok());
        assert!(validate_go_namespace("product_row").is_ok());
        assert!(validate_go_namespace("sum_k0_case").is_ok());
    }

    #[test]
    fn swift_default_mangles_support_brand_collisions() {
        assert_eq!(default_swift_namespace("kio_unit"), "KioPkg_KioUnit");
        assert_eq!(default_swift_namespace("product"), "KioPkg_Product");
        assert_eq!(default_swift_namespace("sum"), "KioPkg_Sum");
        assert_eq!(default_swift_namespace("kio_apply1"), "KioPkg_KioApply1");
        assert_eq!(default_swift_namespace("kio_native"), "KioPkg_KioNative");
        assert_eq!(
            default_swift_namespace("kio_native_constructor"),
            "KioPkg_KioNativeConstructor"
        );
    }

    #[test]
    fn swift_explicit_value_rejects_support_brands() {
        assert!(validate_swift_namespace("KioUnit").is_err());
        assert!(validate_swift_namespace("Product").is_err());
        assert!(validate_swift_namespace("Sum").is_err());
        assert!(validate_swift_namespace("KioApply2").is_err());
        assert!(validate_swift_namespace("KioNative").is_err());
        assert!(validate_swift_namespace("KioNativeConstructor").is_err());
        assert!(validate_swift_namespace("KioHostTypeIdentity_api__Box").is_err());
        assert!(validate_swift_namespace("KioFacade_V1_Product_K2_P0_P1").is_err());
        assert!(validate_swift_namespace("KioHostType_api__Box").is_err());
        assert!(validate_swift_namespace("KioHostTypeMk_api__Box").is_err());
        assert!(validate_swift_namespace("KioNewtype_api__Token").is_err());
        assert!(validate_swift_namespace("KioNewtypeMk_api__Token").is_err());
        assert!(validate_swift_namespace("KioForall_api__HostFn_apply_U0").is_err());
        assert!(validate_swift_namespace("KioUnits").is_ok());
        assert!(validate_swift_namespace("Products").is_ok());
        assert!(validate_swift_namespace("KioApplyReader").is_ok());
        assert!(validate_swift_namespace("KioFacades").is_ok());
    }

    #[test]
    fn rust_explicit_value_rejects_empty_brand() {
        assert!(validate_rust_namespace("_").is_err());
        assert!(validate_rust_namespace("___").is_err());
    }
}

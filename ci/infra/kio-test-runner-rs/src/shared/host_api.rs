//! Backend-agnostic data shapes representing the host API the
//! package's host declares.
//!
//! The types here are deliberately a *runner-local minimal* shape:
//! they carry just enough to render the structured host types and
//! functions supplied by one exact protocol. Each runner starts its
//! `HostApi` from the selected **protocol** — the complete host and
//! execution contract — reading
//! no `.kio` source and no host declarations, signatures, or structural
//! shapes from `kio build` output. A protocol is exact, not a capability
//! superset: its host types and functions are already the complete set
//! the runner must implement.

use crate::protocol::{HostFnBinding, HostTypeBinding, HostTypeFixture, ProtocolContract};

#[allow(dead_code)]
pub fn host_name_core(source: &str) -> String {
    let start = source.len() - source.trim_start_matches('_').len();
    let end = source.trim_end_matches('_').len().max(start);
    let mut rendered = source[..start].to_owned();
    let mut capitalize = false;
    for ch in source[start..end].chars() {
        if ch == '_' {
            capitalize = true;
        } else {
            rendered.push(if capitalize {
                ch.to_ascii_uppercase()
            } else {
                ch
            });
            capitalize = false;
        }
    }
    rendered.push_str(&source[end..]);
    rendered
}

/// One associated type the package's host trait declares.
// Cargo's all-feature/all-target check compiles the shared Rust protocol
// renderer into the dyn runner, which uses `TraitMethod` but does not construct
// the static runners' two aggregate host-API shapes.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocType {
    /// The protocol-owned semantic leaf (`Int`, `Str`, …).
    pub name: String,
    /// The exact emitted boundary member when the protocol must distinguish
    /// declarations with the same leaf in different modules. `None` means the
    /// declaration uses its bare leaf. Keeping this identity explicit avoids
    /// treating a legal `__` affix in a source identifier as evidence that the name
    /// has already been qualified.
    pub boundary_name: Option<String>,
    /// `i32`, `str`, … the role declared alongside the type. The
    /// per-backend runner maps this to the host-language-native
    /// type when synthesizing the stub. Empty string for non-role-
    /// typed host types. Every Rust host type renders as an associated type;
    /// the role selects the runner's canonical backing alias rather than the
    /// emitted representation.
    pub role: String,
    /// Type-parameter names the associated type carries, in
    /// declaration order. Empty for arity-0 host types
    /// (`type Foo;`); populated for polymorphic ones
    /// (`type Array[T];` produces `["T"]`). Role-typed host
    /// types are always arity-0.
    pub type_params: Vec<String>,
}

/// One method the package's host trait declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitMethod {
    /// The fn name as it appears in the emitted host trait
    /// (`print`, `addI32`, `i32ToString`, `r#loop`, …).
    pub name: String,
    /// Method-level type-parameter names, in declaration order.
    /// Empty for monomorphic methods; populated when the host
    /// fn itself carries `[T]` / `[T][U]` (e.g., `fn id[T](v: T)
    /// -> T;` produces `["T"]`). Bound text is stripped — only the
    /// parameter name remains.
    pub type_params: Vec<String>,
    /// Argument types, verbatim, in declaration order. Each entry
    /// is the backend-specific type expression as it appears in the
    /// emitted source (`Self::Foo`, `t`, `i32`, `crate::shapes::…`).
    /// Per-backend rendering picks this up.
    pub arg_types: Vec<String>,
    /// Return type, or `()` for a unit-returning method.
    pub ret_type: String,
    /// Raw `where` clause text (everything after `where ` up to the
    /// closing `;`), if any. Used by the Rust runner when canonical
    /// host bodies need shape types that only appear in closure
    /// bounds. Empty for runners that don't carry one.
    pub where_clause: String,
}

/// One exact protocol-owned host declaration identity.
///
/// Module and leaf remain separate: both are legal Kio identifiers and
/// flattening them through a separator would make the projection ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostItemIdentity {
    pub module: String,
    pub leaf: String,
}

impl HostItemIdentity {
    #[allow(dead_code)] // Some runner bins only consume an already-projected API.
    pub fn new(module: &str, leaf: &str) -> Self {
        Self {
            module: module.to_owned(),
            leaf: leaf.to_owned(),
        }
    }
}

/// One exact host-type identity paired with its backend rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTypeApi {
    pub identity: HostItemIdentity,
    pub rendered: AssocType,
}

/// One exact host-function identity paired with its backend rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFunctionApi {
    pub identity: HostItemIdentity,
    pub rendered: TraitMethod,
}

/// Backend-agnostic summary of one package's host API.
///
/// Each entry is a single declaration the host commits to:
/// `types` lists every role-bearing or roleless host-type declaration
/// (including parameterized ones), and `functions` lists every host-fn
/// declaration. Each backend rendering remains inseparable from the exact
/// qualified protocol identity it implements.
///
/// The redesigned host boundary is **module-namespaced**: the emitted
/// Rust and Swift name exact host members `<encoded-module>__<leaf>` in their
/// host trait/protocol, and the JS host record nests each item under
/// `__host__.<MODULE_NS>.<leaf>`. The runner reconstructs that
/// namespacing from each protocol binding's exact module and leaf, not
/// from an emitted signature or a global leaf-to-module map.
/// Protocol-owned [`AssocType::name`] entries keep their bare semantic leaf.
/// [`TraitMethod::name`] may likewise carry the emitted namespaced spelling;
/// behavior remains attached to the protocol binding and is never recovered
/// from that spelling.
#[allow(dead_code)] // See the per-binary all-feature note on `AssocType`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApi {
    pub types: Vec<HostTypeApi>,
    pub functions: Vec<HostFunctionApi>,
}

impl HostApi {
    #[allow(dead_code)] // Dynamically typed runners consume structured identities directly.
    pub fn assoc_types(&self) -> impl ExactSizeIterator<Item = &AssocType> {
        self.types.iter().map(|entry| &entry.rendered)
    }

    #[allow(dead_code)] // Compile-only runner bins do not render host functions.
    pub fn methods(&self) -> impl ExactSizeIterator<Item = &TraitMethod> {
        self.functions.iter().map(|entry| &entry.rendered)
    }
}

/// Pair one backend's rendered host declarations with a protocol's exact
/// qualified identities.
///
/// The inventory comes directly from the protocol, and each renderer receives
/// the same binding whose identity is stored beside its result. There are no
/// separately ordered identity and rendering vectors that can shift.
#[allow(dead_code)] // Dynamically typed runners use the wrapper below.
pub fn project_host_api(
    contract: ProtocolContract,
    mut render_type: impl FnMut(&HostTypeBinding) -> AssocType,
    mut render_function: impl FnMut(&HostFnBinding) -> TraitMethod,
) -> HostApi {
    let mut seen_types = std::collections::BTreeSet::new();
    let types = contract
        .host_types
        .iter()
        .map(|binding| {
            assert!(
                seen_types.insert((binding.module, binding.leaf)),
                "duplicate protocol host type `{}/{}`",
                binding.module,
                binding.leaf
            );
            HostTypeApi {
                identity: HostItemIdentity::new(binding.module, binding.leaf),
                rendered: render_type(binding),
            }
        })
        .collect();

    let mut seen_functions = std::collections::BTreeSet::new();
    let functions = contract
        .host_fns
        .iter()
        .map(|binding| {
            assert!(
                seen_functions.insert((binding.module, binding.leaf)),
                "duplicate protocol host function `{}/{}`",
                binding.module,
                binding.leaf
            );
            HostFunctionApi {
                identity: HostItemIdentity::new(binding.module, binding.leaf),
                rendered: render_function(binding),
            }
        })
        .collect();

    HostApi { types, functions }
}

/// Recheck that a projected API belongs to one exact protocol before a runner
/// combines it with the protocol's structured body recipes.
#[allow(dead_code)] // Native typed runners construct and consume one projection directly.
pub fn assert_host_api(contract: ProtocolContract, host: &HostApi) {
    assert_eq!(
        host.types.len(),
        contract.host_types.len(),
        "projected host-type inventory does not match the selected protocol"
    );
    assert_eq!(
        host.functions.len(),
        contract.host_fns.len(),
        "projected host-function inventory does not match the selected protocol"
    );
    for (entry, binding) in host.types.iter().zip(contract.host_types) {
        assert_eq!(entry.identity.module, binding.module);
        assert_eq!(entry.identity.leaf, binding.leaf);
    }
    for (entry, binding) in host.functions.iter().zip(contract.host_fns) {
        assert_eq!(entry.identity.module, binding.module);
        assert_eq!(entry.identity.leaf, binding.leaf);
    }
}

/// Project an exact protocol for a dynamically typed host surface.
///
/// JS and Python need no native signature strings, but they still carry one
/// rendered entry for every exact type and function declaration. Runtime
/// rendering consumes the paired qualified identities and the protocol's
/// structured body recipes.
#[allow(dead_code)] // Native typed runners provide their own native rendering.
pub fn dynamic_host_api(contract: ProtocolContract) -> HostApi {
    project_host_api(
        contract,
        |binding| AssocType {
            name: binding.leaf.to_owned(),
            boundary_name: None,
            role: match binding.fixture {
                HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => {
                    role.role().to_owned()
                }
                _ => String::new(),
            },
            type_params: (0..binding.type_arity)
                .map(|index| format!("T{index}"))
                .collect(),
        },
        |binding| TraitMethod {
            name: binding.leaf.to_owned(),
            type_params: Vec::new(),
            arg_types: Vec::new(),
            ret_type: String::new(),
            where_clause: String::new(),
        },
    )
}

#[cfg(test)]
mod projection_tests {
    use super::{assert_host_api, dynamic_host_api};
    use crate::protocol::RunnerProtocol;

    #[test]
    fn dynamic_host_apis_preserve_every_exact_protocol_identity() {
        for &protocol in RunnerProtocol::ALL {
            let contract = protocol.contract();
            let host = dynamic_host_api(contract);
            assert_host_api(contract, &host);

            for (entry, binding) in host.types.iter().zip(contract.host_types) {
                assert_eq!(entry.rendered.name, binding.leaf, "{protocol:?}");
                assert_eq!(
                    entry.rendered.type_params.len(),
                    usize::from(binding.type_arity),
                    "{protocol:?}"
                );
            }
            for (entry, binding) in host.functions.iter().zip(contract.host_fns) {
                assert_eq!(entry.rendered.name, binding.leaf, "{protocol:?}");
            }
        }
    }
}

/// The Rust host-boundary name for one declaring module + leaf. Module
/// Paths without source underscores keep `/` → `_`. Paths containing `_`
/// instead use the reserved `__kio_host_` form, encoding `_` as `_u` and `/`
/// as `_s`; `__` separates the module from the word-cased leaf. Mirrors
/// `kio-rs`'s `mangle_host_name`.
#[cfg(feature = "rust")]
#[allow(dead_code)]
pub fn rust_host_member(module: &str, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module.contains('_') {
        return format!("{}__{leaf}", module.replace('/', "_"));
    }
    let encoded = format!("__kio_host_{}", encode_host_identity(module));
    format!("{encoded}__{leaf}")
}

/// The Go host-boundary method name for a declaring module + leaf. Paths
/// without source underscores retain the readable export-capitalized form.
/// Paths containing underscores use the emitter's reserved `KioItem_` form,
/// encoding `_` as `_u` and `/` as `_s` before the `__` leaf separator.
#[cfg(feature = "go")]
#[allow(dead_code)]
pub fn go_host_member(module: &str, leaf: &str) -> String {
    go_qualified_boundary_member(module, None, leaf)
}

/// The Go boundary member name for an item owned by `module` and an optional
/// public-newtype `qualifier`. The runner derives `ffi.go` aliases from the
/// protocol's fixed source identities; it does not inspect emitted source.
///
/// A slash-only module path and underscore-free qualifier keep the readable
/// form. If either owner component contains `_`, each owner component is
/// encoded independently (`_` → `_u`, `/` → `_s`) under `KioItem_`, with
/// `__` preserving the component boundaries. Mirrors
/// `kio-rs`'s `qualified_boundary_member_name`.
#[cfg(feature = "go")]
pub fn go_qualified_boundary_member(module: &str, qualifier: Option<&str>, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module.contains('_') && qualifier.is_none_or(|q| !q.contains('_')) {
        let flat_module = module.replace('/', "_");
        return match qualifier {
            Some(q) => go_export_capitalize(&format!("{flat_module}__{q}_{leaf}")),
            None => go_export_capitalize(&format!("{flat_module}__{leaf}")),
        };
    }

    let mut encoded_owner = go_encode_host_identity(module);
    if let Some(q) = qualifier {
        encoded_owner.push_str("__");
        encoded_owner.push_str(&go_encode_host_identity(q));
    }
    format!("KioItem_{encoded_owner}__{leaf}")
}

/// The selector for a nested module in the emitted Go package facade.
///
/// Root module selectors keep their conventional exported spelling; only a
/// module below that root uses this role-tagged class.
#[cfg(feature = "go")]
#[allow(dead_code)]
pub fn go_nested_module_selector(source: &str) -> String {
    format!("KioModule_{}", go_encode_host_identity(source))
}

/// The selector for a public-newtype handle in the emitted Go package facade.
#[cfg(feature = "go")]
#[allow(dead_code)]
pub fn go_type_handle_selector(source: &str) -> String {
    format!("KioType_{}", go_encode_host_identity(source))
}

#[cfg(feature = "go")]
fn go_encode_host_identity(source: &str) -> String {
    encode_host_identity(source)
}

/// Capitalize the first ASCII character of `s`, leaving the rest
/// unchanged — the Go export rule the emitter's `capitalize_first`
/// applies to a mangled boundary name. Used for bare (non-namespaced)
/// host-fn method names too.
#[cfg(feature = "go")]
#[allow(dead_code)]
pub fn go_export_capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(any(
    feature = "rust",
    feature = "go",
    feature = "java",
    feature = "swift",
    feature = "js",
    feature = "ts",
    feature = "python"
))]
fn encode_host_identity(source: &str) -> String {
    let source = source
        .split('/')
        .map(host_name_core)
        .collect::<Vec<_>>()
        .join("/");
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

/// One module selector in the generated package facade.
///
/// The first component uses its word-cased spelling. Every nested component
/// carries the module role explicitly, so a module and public-newtype handle
/// with the same source spelling remain distinct. Mirrors the emitters'
/// `FacadeSelector::facade_name`.
#[cfg(any(
    feature = "java",
    feature = "swift",
    feature = "js",
    feature = "ts",
    feature = "python"
))]
#[allow(dead_code)]
pub fn facade_module_selector(source: &str, root: bool) -> String {
    if root {
        host_name_core(source)
    } else {
        format!("KioModule_{}", encode_host_identity(source))
    }
}

/// One public-newtype handle selector in the generated package facade.
#[cfg(any(
    feature = "java",
    feature = "swift",
    feature = "js",
    feature = "ts",
    feature = "python"
))]
#[allow(dead_code)]
pub fn facade_type_selector(source: &str) -> String {
    format!("KioType_{}", encode_host_identity(source))
}

/// Public host-record key for one exact declaring module.
///
/// Slash-only paths keep the readable lowercase `/` → `_` form. Paths
/// containing a source underscore use an exact reserved class so `_` and `/`
/// never collapse to the same key.
#[cfg(any(feature = "js", feature = "ts", feature = "python"))]
#[allow(dead_code)]
pub fn host_module_key(module: &str) -> String {
    if !module.contains('_') {
        return module.replace('/', "_");
    }
    format!("KioModule_{}", encode_host_identity(module))
}

/// The Java host-boundary method name for a declaring module + leaf.
/// Slash-only paths keep the readable `/` → `_` form; paths containing a
/// source underscore use the exact `KioItem_...` class. Mirrors the Java
/// emitter's `host_member_name`.
#[cfg(feature = "java")]
#[allow(dead_code)]
pub fn java_host_member(module: &str, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module.contains('_') {
        return format!("{}__{leaf}", module.replace('/', "_"));
    }
    format!("KioItem_{}__{leaf}", encode_host_identity(module))
}

/// The Swift host-boundary method name for a declaring module + leaf.
/// Slash-only paths keep the readable `/` → `_` form; paths containing a
/// source underscore use the exact `KioItem_...` class. Mirrors the Swift
/// emitter's host-member naming.
#[cfg(feature = "swift")]
#[allow(dead_code)]
pub fn swift_host_member(module: &str, leaf: &str) -> String {
    swift_host_member_rendered(module, &host_name_core(leaf))
}

#[cfg(feature = "swift")]
fn swift_host_member_rendered(module: &str, leaf: &str) -> String {
    if !module.contains('_') {
        return format!("{}__{leaf}", module.replace('/', "_"));
    }
    format!("KioItem_{}__{leaf}", encode_host_identity(module))
}

/// The Swift `ffi.swift` member for one public newtype constructor or
/// projector. Mirrors the emitter independently, retaining module, newtype,
/// member, and semantic role as separate components.
#[cfg(feature = "swift")]
#[allow(dead_code)]
pub fn swift_export_newtype_member(module: &str, newtype: &str, member: &str) -> String {
    let leaf = format!(
        "__KioType_{}__KioItem_{}",
        encode_host_identity(newtype),
        encode_host_identity(member)
    );
    swift_host_member_rendered(module, &leaf)
}

/// The Swift exact-host-type member for a declaring module + leaf. The
/// injective encoding mirrors the Swift emitter independently: readable
/// slash replacement for paths without underscores, and a reserved framed
/// form when underscores make that spelling ambiguous.
#[cfg(feature = "swift")]
#[allow(dead_code)]
pub fn swift_host_type_member(module: &str, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module.contains('_') {
        return format!("{}__{leaf}", module.replace('/', "_"));
    }
    let encoded = format!("__kio_host_{}", encode_host_identity(module));
    format!("{encoded}__{leaf}")
}

/// The JS/TS host-record namespace for a declaring module. Slash-only paths
/// keep the readable lowercase `/` → `_` form; paths containing a source
/// underscore use the exact `KioModule_...` class. Mirrors
/// `kio-rs`'s `host_module_key`.
#[cfg(any(feature = "js", feature = "ts"))]
#[allow(dead_code)]
pub fn js_host_namespace(module: &str) -> String {
    host_module_key(module)
}

#[cfg(all(
    test,
    any(feature = "rust", feature = "go", feature = "java", feature = "swift")
))]
mod tests {
    use super::*;

    #[cfg(feature = "rust")]
    #[test]
    fn rust_host_member_preserves_legal_identity() {
        assert_eq!(rust_host_member("app", "print"), "app__print");
        assert_eq!(rust_host_member("util/io", "print"), "util_io__print");
        assert_eq!(rust_host_member("a_b_", "c"), "__kio_host_aB_u__c");
        assert_ne!(rust_host_member("a_b_", "c"), rust_host_member("a", "b_c_"));
        assert_ne!(
            rust_host_member("a_b/c", "X"),
            rust_host_member("a/b_c", "X")
        );
    }

    #[cfg(feature = "go")]
    #[test]
    fn go_host_member_preserves_legal_identity() {
        assert_eq!(go_host_member("app", "print"), "App__print");
        assert_eq!(go_host_member("util/io", "print"), "Util_io__print");
        assert_eq!(go_host_member("a_b_", "c"), "KioItem_aB_u__c");
        assert_ne!(go_host_member("a_b_", "c"), go_host_member("a", "b_c_"));
        assert_ne!(go_host_member("a_b/c", "x"), go_host_member("a/b_c", "x"));
    }

    #[cfg(feature = "go")]
    #[test]
    fn go_qualified_boundary_member_preserves_owner_boundaries() {
        assert_eq!(
            go_qualified_boundary_member("api", Some("A_b"), "c"),
            "KioItem_api__AB__c"
        );
        assert_eq!(
            go_qualified_boundary_member("api", Some("A"), "b_c"),
            "Api__A_bC"
        );
        assert_ne!(
            go_qualified_boundary_member("api", Some("A_b"), "c"),
            go_qualified_boundary_member("api", Some("A"), "b_c")
        );
        assert_eq!(
            go_qualified_boundary_member(
                "testapi/types",
                Some("Constructor_pair"),
                "make_constructor_pair"
            ),
            "KioItem_testapi_stypes__ConstructorPair__makeConstructorPair"
        );
        assert_eq!(
            go_nested_module_selector("foo_bar__"),
            "KioModule_fooBar_u_u"
        );
        assert_eq!(go_type_handle_selector("Foo_bar"), "KioType_FooBar");
        assert_eq!(go_type_handle_selector("_Box"), "KioType__uBox");
        assert_ne!(
            go_nested_module_selector("child"),
            go_type_handle_selector("Child")
        );
    }

    #[cfg(feature = "java")]
    #[test]
    fn java_host_member_preserves_legal_identity() {
        assert_eq!(java_host_member("foo/bar", "read"), "foo_bar__read");
        assert_eq!(java_host_member("foo_bar", "read"), "KioItem_fooBar__read");
        assert_ne!(
            java_host_member("foo/bar", "read"),
            java_host_member("foo_bar", "read")
        );
    }

    #[cfg(feature = "swift")]
    #[test]
    fn swift_host_member_preserves_legal_identity() {
        assert_eq!(swift_host_member("foo/bar", "read"), "foo_bar__read");
        assert_eq!(swift_host_member("foo_bar", "read"), "KioItem_fooBar__read");
        assert_ne!(
            swift_host_member("foo/bar", "read"),
            swift_host_member("foo_bar", "read")
        );
        assert_eq!(
            swift_export_newtype_member("main", "A_b", "c"),
            "main____KioType_AB__KioItem_c"
        );
        assert_ne!(
            swift_export_newtype_member("main", "A_b", "c"),
            swift_export_newtype_member("main", "A", "b_c")
        );
    }

    #[cfg(feature = "swift")]
    #[test]
    fn swift_host_type_member_preserves_legal_identity() {
        assert_eq!(swift_host_type_member("app", "Token"), "app__Token");
        assert_eq!(swift_host_type_member("util/io", "Token"), "util_io__Token");
        assert_eq!(swift_host_type_member("a_b_", "c"), "__kio_host_aB_u__c");
        assert_ne!(
            swift_host_type_member("a_b_", "c"),
            swift_host_type_member("a", "b_c_")
        );
        assert_ne!(
            swift_host_type_member("a_b/c", "X"),
            swift_host_type_member("a/b_c", "X")
        );
    }
}

#[cfg(all(test, any(feature = "js", feature = "ts", feature = "python")))]
mod host_module_key_tests {
    use super::{facade_module_selector, facade_type_selector, host_module_key};

    #[test]
    fn public_host_key_preserves_legal_module_identity() {
        assert_eq!(host_module_key("foo/bar"), "foo_bar");
        assert_eq!(host_module_key("foo_bar"), "KioModule_fooBar");
        assert_ne!(host_module_key("foo/bar"), host_module_key("foo_bar"));
    }

    #[test]
    fn facade_selectors_preserve_roles_and_source_identity() {
        assert_eq!(facade_module_selector("api", true), "api");
        assert_eq!(facade_module_selector("foo_bar", false), "KioModule_fooBar");
        assert_eq!(facade_type_selector("Foo_bar"), "KioType_FooBar");
        assert_eq!(facade_type_selector("_Box"), "KioType__uBox");
        assert_ne!(
            facade_module_selector("foo_bar", false),
            facade_type_selector("Foo_bar")
        );
    }
}

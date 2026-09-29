//! Per-backend codegen — profiles and lowerings.
//!
//! The shared, backend-agnostic work is [`crate::pass::structural_recovery`]
//! followed by [`crate::pass::recover_to_low`]: the structural-recovery pass
//! collapses right-leaning intrinsic chains into the `Expr::Enriched*`
//! n-ary nodes, and the resolution-lowering pass classifies every
//! call site into one of the `Expr::Low*` variants on the `Routed`
//! phase. Per-backend lowerings consume `Module<Routed>` and dispatch
//! over the pre-classified variants. Both the JS and Rust backends
//! consume `Module<Routed>` directly.
//!
//! What's **per-backend** is twofold:
//!
//! 1. A [`Profile`] — a declarative descriptor of what the host
//!    language can natively express (named records, sums-with-payload,
//!    pattern matching, …). Lowerings ship a profile alongside their
//!    code; the profile is the cost-model bookkeeping a new backend
//!    fills in when joining the framework.
//! 2. A **lowering** — a backend-specific translation from the Routed
//!    IR to idiomatic host-language source. The lowering takes a
//!    `Package<Routed>` and returns the emitted source files. Where the
//!    lowering pads (single-file factory module, per-module Rust
//!    files, an output directory, …) is the backend's concern.
//!
//! The boundary shape — what a host sees at the FFI — is settled
//! separately in [`specs/backends/<lang>.md`](../../../specs/backends/),
//! sharing the cross-cutting framework in
//! [`specs/backends/README.md`](../../../specs/backends/README.md) (right-
//! spine walk, 3-step key fallback, generic semantic-key shell identity). A
//! per-backend lowering's per-signature wrapper bridges its internal
//! rep to that FFI shape.
//!
//! ## Per-module parallelism (framework convention)
//!
//! Every per-backend `lower_package_*` entry point **must** fan out
//! across the package's modules via rayon's `par_iter` at the
//! package-level driver. The post-typecheck IR pipeline already does
//! this for [`crate::pass::structural_recovery::recover_package`] and
//! [`crate::pass::optimize::optimize_package`]; per-backend lowering is the
//! tail of that pipeline and inherits the same per-module grain.
//!
//! Carve-out: this convention binds backends that emit per-module host-language
//! source. Interpreter-style backends ([`python`], [`java`]) serialize the
//! whole typed IR to JSON and ship a runtime interpreter instead of
//! generating per-module code, so they have nothing per-module to
//! parallelize and legitimately don't `par_iter` — not a performance
//! regression.
//!
//! Why per-module: each module's lowering reads the package's
//! cross-module side tables (use-imports, host-fn signatures, shape
//! registry) immutably and produces a self-contained piece of host-language
//! source. The pieces are concatenated in deterministic
//! `BTreeMap`-iteration order, so byte-stable output is preserved.
//!
//! - **JS** ([`js`]). [`crate::backends::js::emit::lower_package_to_factory_module`]
//!   fans the per-module IIFE production across rayon — each module
//!   yields one `const __mod_<...> = (() => { … })();` block, and the
//!   factory body concatenates them in input order. Consumes
//!   `Module<Routed>`; per-variant render arms dispatch over the
//!   `Expr::Low*` family.
//! - **Rust** ([`rust`]). [`crate::backends::rust::emit::lower_package`]
//!   groups module fns by `module_path` and emits each group's
//!   methods in parallel, against per-module use-imports built once
//!   per group. The aggregated `lib.rs` retains the serial walk's
//!   method ordering byte-for-byte.
//!
//! New backends joining the framework wire their own per-module
//! fan-out from day one.
//!
//! ## Runtime-support library convention
//!
//! Some backends need fragments of host-language source that the
//! lowering relies on existing — wrapper helpers that turn host- /
//! module-fn references into the host language's first-class fn value, the
//! existential erasure / recovery helpers used when a `newtype`
//! seals a type-parameter, the polymorphic identity helper used by
//! `[T]`-quantified trivial bodies, etc. Inlining the bodies of
//! these helpers at every call site bloats the emit and complicates
//! cross-pass diffs.
//!
//! The framework's convention is for each lowering to declare what
//! it needs via its [`Profile::runtime_support`] field, whose two arms are:
//!
//! - [`RuntimeSupport::None`] — the framework writes no fixed-path support
//!   file. The backend may place private declarations in its facade, emit
//!   expressions directly at their use sites, or manage namespace-varying
//!   support files itself. Haskell, JS, and Java respectively exercise those
//!   three approaches.
//! - [`RuntimeSupport::EmbeddedFile`] — the lowering ships a fixed
//!   host-language source file alongside the package's main
//!   files. Per-variant emit calls named functions defined in that
//!   file rather than inlining the bodies. Rust picks this — its
//!   strict typing forces a non-trivial `Rc<dyn Fn>` /
//!   `Rc<dyn Any>` wrap that's worth naming once. The Rust backend
//!   ships [`rust::RUNTIME_SUPPORT_FILE_CONTENT`] as
//!   `src/__kio_runtime.rs` in every emitted crate.
//!
//! The file's *content* is the per-backend submodule's concern; the
//! *convention* — that there is one fixed file, written by the
//! emitter, not user-modifiable — is the framework's.
//!
//! ## Backends
//!
//! - [`js`] — JavaScript. The emitter consumes `Package<Routed>` and writes a
//!   single `<ns>.js` ES module exporting a branded
//!   `create<Handle>(host)` factory (the namespace/stem defaults to the
//!   package name; the handle is its PascalCase).
//! - [`ts`] — TypeScript. A pure typed skin over the JS backend:
//!   byte-identical `<ns>.js` plus a generated `<ns>.d.ts`.
//! - [`go`] — Go. An **erased body** over `any` / `[]any` (the JS
//!   dynamic body's shape on a statically-typed host — higher-kinded
//!   values ride the uniform `[]any` rep) plus a
//!   typed FFI facade (generic row-anchored sums, generic struct products),
//!   emitted as a multi-file Go package exposing a branded
//!   `Create<Handle>` factory under the package's namespace.
//! - [`java`] — Java. An erased body over `Object` (a serialized-IR
//!   interpreter) plus a generated typed facade, emitted as four source
//!   files under the package-namespace directory.
//! - [`python`] — Python. A type-erased dynamic body over ordinary Python
//!   values plus a keyed-dictionary FFI skin, emitted as a single
//!   `<ns>.py` module (the stem is the package namespace) exposing a
//!   branded `create_<ns>(host)` factory.
//! - [`rust`] — Rust. An erased body over `Rc<dyn Any>` plus a typed FFI
//!   skin, emitted as a Cargo crate exposing a branded `create_<crate>` factory.
//! - [`swift`] — Swift. An erased body over `Any` / `[Any]` plus a typed
//!   FFI skin (native `enum` sums, `struct` products), emitted as a
//!   multi-file Swift module exposing a branded `create<Handle>(host:)`
//!   factory under the package's namespace — the module name, imposed via
//!   `-module-name` and published in the `pkg.swift` marker line.
//! - [`haskell`] — Haskell. The native-HKT backend using GHC's kind and
//!   rank-N support, with strictness inserted to preserve Kio evaluation,
//!   emitted as a namespaced Haskell module exposing a branded
//!   `create<Handle>` factory under the package's namespace.

pub mod boundary_facade;
pub mod go;
pub mod haskell;
pub mod java;
pub mod js;
pub mod kio_prime;
pub mod namespace;
pub(crate) mod public_names;
pub mod python;
pub mod reconstruct;
pub mod rust;
pub(crate) mod serialized_runtime_ir;
pub mod skin;
pub mod structural;
pub mod swift;
pub mod ts;

/// Map each selectively imported module function to its declaring module.
///
/// Name resolution has already validated the import, but emitters still need
/// this table to turn a bare `LowModuleCall` back into the target backend
/// symbol. Rechecking with the resolver's caller-aware visibility predicate
/// preserves `pub(path)` imports without admitting scoped items elsewhere.
pub(crate) fn selective_module_fn_import_owners(
    importer: &crate::ast::Module<crate::ast::Routed>,
    package: &crate::pass::resolve::Package<crate::ast::Routed>,
) -> std::collections::BTreeMap<String, String> {
    let mut owners = std::collections::BTreeMap::new();
    for import_decl in &importer.imports {
        let (items, from) = match &import_decl.kind {
            crate::ast::ImportKind::Selective { items, from } => (items, from),
            _ => continue,
        };
        let owner = from
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let Some(target) = package.module(&owner) else {
            continue;
        };
        for item in items {
            let Some(name) = item.as_name() else {
                continue;
            };
            let Some(definition) =
                target
                    .module
                    .items
                    .iter()
                    .find_map(|candidate| match candidate {
                        crate::ast::Item::FnDef(definition) if definition.name == name => {
                            Some(definition)
                        }
                        _ => None,
                    })
            else {
                continue;
            };
            if crate::pass::resolve::is_visible(&definition.vis, &importer.path) {
                owners.insert(name.to_owned(), owner.clone());
            }
        }
    }
    owners
}

/// What a host language can natively express, for the purposes of
/// the *internal rep and idiomatic emit*. Descriptive — the framework
/// does not enforce any of these axes; lowerings consult their own
/// profile (and, in the future, shared optimization passes will
/// consult per-backend profiles to vary IR-to-backend rules).
///
/// The FFI-shape obligations (what the host sees at the boundary) are
/// independent of `Profile` — those are settled per
/// `specs/backends/<lang>.md` and the cross-cutting framework in
/// `specs/backends/README.md`. The `Profile` is about what the *host language*
/// can natively absorb beneath the FFI wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// `true` when the host language can express records with named fields
    /// natively (Rust `struct`, JS object, Python dataclass, …).
    /// Lowerings without native records render
    /// [`crate::ast::Expr::EnrichedRecord`] positionally — same as
    /// [`crate::ast::Expr::EnrichedTuple`] — and surface the names at
    /// the FFI wrapper instead.
    pub native_records: bool,

    /// `true` when the host language can express sums-with-payload natively
    /// (Rust / Swift / OCaml `enum`, sealed-interface-style sums).
    /// Lowerings without native sums render
    /// [`crate::ast::Expr::EnrichedInject`] / `EnrichedMatch` via an
    /// interface + per-variant struct or a `[tag, payload]` dispatch.
    pub native_sums: bool,

    /// `true` when the host language has native pattern matching (Rust
    /// `match`, Swift `switch`, Python `match`/`case`). Lowerings
    /// without it render [`crate::ast::Expr::EnrichedMatch`] as a
    /// tag-dispatch ternary chain or a switch on the tag field.
    pub native_match: bool,

    /// How the host language expresses field access. Determines the
    /// idiomatic emit shape for `EnrichedProject` / `EnrichedFieldGet`
    /// after the internal rep is chosen.
    pub field_access: FieldAccessStyle,

    /// `true` when the host language requires types to be declared upfront
    /// (Rust `struct` / `enum`, class-shaped declarations). The lowering's emit
    /// includes type declarations alongside the value emit. For
    /// structural / duck-typed backends (JS, Python) the value emit
    /// stands alone.
    pub requires_explicit_type_decls: bool,

    /// `true` when the host language is garbage-collected. Determines
    /// whether the emit includes ownership annotations (Rust `Box`,
    /// `Rc`, lifetime parameters) for shared values.
    pub gc: bool,

    /// What runtime-support primitives the lowering relies on
    /// existing. See the module's [§ Runtime-support library
    /// convention](self) for the rationale and the per-backend wiring.
    pub runtime_support: RuntimeSupport,
}

/// Whether a backend's lowering relies on an emitter-shipped runtime
/// support file holding wrap / erase / identity helpers, and (if so)
/// what filename to write it under. See the module's [§ Runtime-
/// support library convention](self).
///
/// The per-backend submodule owns the file's content; this enum is the
/// framework-level handshake — "write this fixed-path file" vs. "the backend
/// manages its support without that framework file".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSupport {
    /// The framework writes no fixed-path support file. The backend may emit
    /// helpers in a facade, inline them, or manage namespace-varying files.
    None,
    /// The lowering ships a fixed host-language source file the
    /// emitter writes alongside the package's main source files.
    /// The relative path is the path under the emitted output
    /// directory (Rust: `src/__kio_runtime.rs`). The file is not user-modifiable —
    /// the emitter rewrites it on every build.
    EmbeddedFile {
        /// Relative path under the emitted output directory, e.g.
        /// `src/__kio_runtime.rs`.
        path: &'static str,
    },
}

/// How the host language expresses field access at the source
/// level. Drives the idiomatic emit for
/// [`crate::ast::Expr::EnrichedProject`] and
/// [`crate::ast::Expr::EnrichedFieldGet`] once the internal rep is
/// chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAccessStyle {
    /// Positional field access via numeric suffix: Rust `t.0`,
    /// `t.1`, … Native-tuple-friendly.
    Numeric,
    /// Named-field access: Rust `s.field`, Python `obj.field`. Pairs
    /// with `native_records = true`; lowerings without native records
    /// fall back to one of the other styles for the positional shape
    /// underneath their FFI wrapper.
    Named,
    /// Bracket / subscript access: JS `arr[0]`, Python `d["k"]`.
    /// Structural / duck-typed backends typically use this for the
    /// positional shape.
    Bracket,
    /// Method-style access: Swift's `t.fst()`, OCaml's `fst t`. Less
    /// common at the value-emit level; included for completeness.
    Method,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JS profile mirrors the JS emitter's current behaviour —
    /// nested-binary array internal rep, bracket access, no native
    /// sums or pattern matching, GC-friendly. The profile is the
    /// contract a new backend reads when joining the framework, so
    /// pinning it here catches an accidental drift.
    #[test]
    fn js_profile_matches_emitter_semantics() {
        let p = js::profile();
        assert!(p.native_records, "JS objects are records");
        assert!(!p.native_sums, "JS has no sums-with-payload primitive");
        assert!(!p.native_match, "JS has no pattern-match");
        assert_eq!(p.field_access, FieldAccessStyle::Bracket);
        assert!(!p.requires_explicit_type_decls);
        assert!(p.gc);
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::None,
            "JS has native fn values and dynamic typing — no runtime-support file needed"
        );
    }

    /// The TS profile mirrors the JS profile: TS is a pure-skin backend
    /// whose runtime body *is* the JS backend's `.js`, byte-identical, so
    /// the profile describes the same dynamic host (object records,
    /// bracket access, no native sums / match, GC, no runtime-support
    /// file). Pinning it here catches a drift away from the JS body it
    /// reuses. The only new artifact is the `.d.ts` skin, which adds no
    /// runtime helpers — hence `RuntimeSupport::None`.
    #[test]
    fn ts_profile_mirrors_js_dynamic_body() {
        let p = ts::profile();
        let js = js::profile();
        assert_eq!(
            p, js,
            "TS reuses the JS `.js` verbatim, so its profile must mirror JS's",
        );
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::None,
            "TS ships the JS `.js` (no support file) and a `.d.ts` (no runtime helpers)",
        );
    }

    /// The Go profile pins the contract from `specs/backends/go.md`: Go
    /// has an **erased body** (the JS dynamic body's shape on a
    /// statically-typed host) with a **typed FFI facade** — native records
    /// (`struct`) but **no** native sums-with-payload (the facade supplies a
    /// generic row-anchored carrier and typed cases) and **no** native pattern
    /// match (hosts switch over `Case()`), named-field access, explicit type
    /// declarations, and GC. Demonstrates the static-no-native-sums facade
    /// axis neither JS nor Rust shows. Go ships an embedded runtime-support
    /// file for the canonical `Unit` and the erased product/sum carrier
    /// adapters.
    #[test]
    fn go_profile_pins_spec_backends_go_md() {
        let p = go::profile();
        assert!(p.native_records, "Go has structs");
        assert!(
            !p.native_sums,
            "Go has no sum-with-payload primitive (the facade supplies KioSum plus typed cases)"
        );
        assert!(
            !p.native_match,
            "Go has no exhaustive pattern-match (hosts type-switch over Case())"
        );
        assert_eq!(p.field_access, FieldAccessStyle::Named);
        assert!(p.requires_explicit_type_decls);
        assert!(p.gc, "Go is garbage-collected");
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::EmbeddedFile {
                path: go::RUNTIME_SUPPORT_FILE_PATH,
            },
            "Go ships a fixed runtime-support file at `kio_runtime.go`",
        );
    }

    /// The Java profile pins the Java backend contract: an erased body
    /// over `Object` (the serialized-IR interpreter), keyed internal
    /// boundary shapes beneath the typed facade, explicit class
    /// declarations, and no framework runtime-support file — the
    /// interpreter ships as one of the backend's own four output files
    /// (`KioRuntime.java`), whose path varies with the package
    /// namespace.
    #[test]
    fn java_profile_pins_spec_backends_java_md() {
        let p = java::profile();
        assert!(
            !p.native_records,
            "Java boundary records cross as keyed KioObject/Map values"
        );
        assert!(
            !p.native_sums,
            "Java boundary sums cross as keyed KioObject/Map values"
        );
        assert!(
            !p.native_match,
            "The Java backend runtime dispatches sums by keys, not native pattern matching"
        );
        assert_eq!(p.field_access, FieldAccessStyle::Bracket);
        assert!(p.requires_explicit_type_decls);
        assert!(p.gc, "Java is garbage-collected");
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::None,
            "Java ships its interpreter as its own namespaced output file, not via the fixed-path EmbeddedFile convention"
        );
    }

    /// The Rust profile pins the contract from
    /// `specs/backends/rust.md`: native records / sums-with-payload /
    /// pattern matching, named-field access, explicit type
    /// declarations, no GC. Demonstrates that the framework
    /// supports backends at the opposite end of the language-feature
    /// spectrum from JS without changing the `Profile` shape.
    #[test]
    fn rust_profile_pins_spec_backends_rust_md() {
        let p = rust::profile();
        assert!(p.native_records, "Rust has structs");
        assert!(p.native_sums, "Rust has enums with payload");
        assert!(p.native_match, "Rust has `match`");
        assert_eq!(p.field_access, FieldAccessStyle::Named);
        assert!(p.requires_explicit_type_decls);
        assert!(!p.gc);
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::EmbeddedFile {
                path: rust::RUNTIME_SUPPORT_FILE_PATH,
            },
            "Rust ships a fixed runtime-support file at `src/__kio_runtime.rs`",
        );
    }

    /// The Swift profile pins the contract from `specs/backends/swift.md`:
    /// an **erased body** (`Any` / `[Any]`, the JS dynamic body's shape on
    /// a statically-typed host) with a **typed FFI skin**
    /// using Swift's native records (`struct`) **and** native
    /// sums-with-payload (`enum` with associated values, matched by an
    /// exhaustive `switch`). The native-sum end of the erased-static
    /// family. Memory is ARC, automatic from the body's view (`gc`).
    #[test]
    fn swift_profile_pins_spec_backends_swift_md() {
        let p = swift::profile();
        assert!(p.native_records, "Swift has structs");
        assert!(p.native_sums, "Swift has enums with associated values");
        assert!(p.native_match, "Swift has exhaustive `switch`");
        assert_eq!(p.field_access, FieldAccessStyle::Named);
        assert!(p.requires_explicit_type_decls);
        assert!(p.gc, "Swift is ARC — automatic from the body's view");
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::EmbeddedFile {
                path: swift::RUNTIME_SUPPORT_FILE_PATH,
            },
            "Swift ships a fixed runtime-support file at `kio_runtime.swift`",
        );
    }

    /// The Haskell profile pins the native-HKT family contract: Haskell
    /// renders the IR at native types (no erasure), so it has native
    /// records (record syntax / tuples) **and** native sums-with-payload
    /// (`data` ADTs, matched by native `case`), named-field access,
    /// explicit type declarations, and GC. Higher-kinded values use
    /// Haskell's own type-constructor application. Its private strictness
    /// helper and universal carrier are unexported declarations in the
    /// package's self-contained `<Ns>.hs` facade, so `runtime_support` is
    /// `None`: the backend writes no separate support file.
    #[test]
    fn haskell_profile_pins_native_hkt_family() {
        let p = haskell::profile();
        assert!(p.native_records, "Haskell has record syntax / tuples");
        assert!(p.native_sums, "Haskell has `data` ADTs with payload");
        assert!(p.native_match, "Haskell has native `case` pattern matching");
        assert_eq!(p.field_access, FieldAccessStyle::Named);
        assert!(p.requires_explicit_type_decls);
        assert!(p.gc, "Haskell is garbage-collected");
        assert_eq!(
            p.runtime_support,
            RuntimeSupport::None,
            "Haskell inlines private runtime support into its self-contained \
             facade rather than shipping a separate runtime-support file"
        );
    }
}

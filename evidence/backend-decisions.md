# Backend decision evidence

This file records backend design rationale and the evidence for its applicability
and conformance. The [specifications](../specs/backends/README.md) remain the
public contract; these records cannot authorize a contract change. Their
conformance conclusions depend on the stated signatures and consumer paths.

These are inherited decision and conformance records, not fresh validation.
Each record retains the available rationale, applicability, evidence anchors
and limits. Any omitted source revision, toolchain version or invocation remains
unrecorded; it cannot be inferred from the current checkout. Conclusions apply
to the stated contract, occurrences and evidence.

## Facade topology and execution provenance

The governing authority is the public contract in
[`specs/backends/README.md`](../specs/backends/README.md): function-type FFI
canonicalization fixes callable stages, and § Deprecated host items already
makes a removal re-emit codegen-only and unreachable from package code. The
facade-topology heading names that combined invariant; it does not change what
host or package code may call.

The semantic predicate is any live host function, exported function, or
public-newtype-member boundary site, plus a removed host function whose frozen
boundary declarations a backend preserves for source stability. The exact
current or frozen signature owns the semantic stages and dependency closure;
only a live site owns package execution authority. A retained callable can
therefore remain syntactically addressable either as an optional deprecated
structural property with no generated body (TypeScript), or as a deprecated
trapping default (Java, Rust, and Swift). A new host must neither implement nor
select it, and it never becomes live package dispatch. Reconstructing either
fact from generated spelling was rejected because it conflates presentation
with semantic identity. This is
open-world: adding an unrelated declaration cannot alter an existing site's
scheme, provenance, or dependencies. It also preserves evaluation because only
the exact live scheme can contribute executable stages.

The propagation ledger separates live callable sites from the retained-removal
occurrence because only the latter varies by backend. The live rows use two
all-backend workflows:
[`ffi_host_poly_function_newtype_roundtrip`](../test-data/goldens/00_success/ffi_host_poly_function_newtype_roundtrip/)
executes a host callback, a public polymorphic-newtype value in both directions,
and a package export, while
[`ffi_newtype_visibility_facade`](../test-data/goldens/00_success/ffi_newtype_visibility_facade/)
exercises host-facing public-newtype-member sites across the visibility
combinations.

| Backend | Occurrence | Applicability | Conformance | Evidence |
| --- | --- | --- | --- | --- |
| JS | live callable sites | applies | conforms — current declarations build the facade and host dispatch | complete workflow — both shared all-backend facade workflows |
| JS | removed host function | not applicable — no frozen declaration is preserved | not applicable — incompatible retained epochs likewise create no generated JS surface | predicate proof — current-only host scan; [`deprecated_host_fn_removal`](../test-data/emissions/js/deprecated_host_fn_removal/) asserts the removed property is absent and the live property remains |
| TypeScript | live callable sites | applies | conforms — the `.d.ts` and shared JS runtime use the current interface | complete workflow — both shared all-backend workflows through the typed facade |
| TypeScript | retained removed host function | applies | conforms with the published incompatible-epoch source-edit caveat — the `.d.ts` keeps each representable exact callable as an optional deprecated property with a visibly deprecated transitive type closure; incompatible same-identity epochs are omitted, while the shared JS runtime stays current-only | complete workflow — [`deprecated_host_fn_source_compatibility`](../test-data/emissions/ts/deprecated_host_fn_source_compatibility/) type-checks unchanged and current-only hosts under `--strict`; [`deprecated_host_fn_shape`](../test-data/emissions/ts/deprecated_host_fn_shape/) checks markers, optionality, transitive support, and runtime inertness; the shared incompatible-epoch witness checks complete root and closure omission |
| Python | live callable sites | applies | conforms — current declarations build namespace entries and validation | complete workflow — both shared all-backend facade workflows |
| Python | history-only host declarations | applies | conforms — Python 3.10 stdlib cannot mark a retained stub declaration deprecated, so both the `.pyi` stub package and `.py` omit the complete history-only closure and current-host validation remains live-only | complete causal workflow — the mixed live/retained unit rejects leaked roots, adapters, carriers, and generic propagation; the sealed-history build test proves a valid history shares the live-only cache while malformed history is still rejected; [`deprecated_host_fn_shape`](../test-data/emissions/python/deprecated_host_fn_shape/) checks both artifacts and [`deprecated_host_fn_live_only`](../test-data/emissions/python/deprecated_host_fn_live_only/) executes a current host |
| Java | live callable sites | applies | conforms — live descriptor methods build the interpreter adapter | complete workflow — both shared all-backend facade workflows |
| Java | retained removed host function and dependencies | applies | conforms with the published source-edit caveats — deprecated throwing defaults and each emitted deprecated transitive dependency remain source-only; removed host roots leave package generics, dependent retained signatures use a deprecated declaration-owned carrier, incompatible epochs omit affected methods/types, and the interpreter adapter stays live-only | complete workflow — focused units cover support, live dominance, deprecated transitive members and current-host optionality; [`deprecated_host_fn_source_compatibility`](../test-data/emissions/java/deprecated_host_fn_source_compatibility/) compiles unchanged and live-only external hosts and runs both through the live export |
| Rust | live callable sites | applies | conforms — live descriptor methods supply package host calls | complete workflow — both shared all-backend facade workflows |
| Rust | retained removed host function and dependencies | applies | conforms when the frozen signature is nameable — the trait default and transitive support are deprecated and inert; a removed associated host type, an incompatible declaration epoch, and every dependent retained surface are omitted under the published source-edit caveats | complete workflow — [`deprecated_host_fn_shape`](../test-data/emissions/rust/deprecated_host_fn_shape/) checks the durable declaration and markers; [`deprecated_host_fn_source_compatibility`](../test-data/emissions/rust/deprecated_host_fn_source_compatibility/) compiles unchanged and live-only hosts and runs both through the live export; focused units check live dominance and dependent omission |
| Go | live callable sites | applies | conforms — the live origin owns bounded converter capabilities | complete workflow — both shared all-backend facade workflows; [`facade_generic_shell_reuse`](../test-data/emissions/go/facade_generic_shell_reuse/) compiles and runs the same host against baseline and unrelated-declaration builds |
| Go | retained removed host function and dependencies | applies | conforms with the published source-edit caveats — every representable retained-only alias, shell, carrier, and constructor has a standard deprecation directive and exposes no live capability; removed host roots leave package generics, and incompatible epochs omit affected roots/types | complete workflow — [`deprecated_host_fn_source_compatibility`](../test-data/emissions/go/deprecated_host_fn_source_compatibility/) compiles unchanged and live-only hosts, runs both through the live export, and checks retained aliases; [`deprecated_host_fn_shape`](../test-data/emissions/go/deprecated_host_fn_shape/) checks markers and absence of the removed method; focused units check live dominance and current-host selection |
| Swift | live callable sites | applies | conforms — the current protocol supplies package dispatch | complete workflow — both shared all-backend facade workflows |
| Swift | retained removed host function and dependencies | applies | conforms with the published incompatible-epoch source-edit caveat — every representable protocol requirement has a deprecated trapping default, removed host types have deprecated concrete defaults, every emitted transitive declaration is deprecated, incompatible epochs omit affected roots/types, and adapters/dispatch remain live-only | complete workflow — focused emitter units and [`deprecated_host_fn_type_compatibility`](../test-data/emissions/swift/deprecated_host_fn_type_compatibility/) check old/current host compilation, markers, defaults, live dominance, and runtime inertness |
| Haskell | live callable sites | applies | conforms — the native host record and body use live functions only | complete workflow — both shared all-backend facade workflows |
| Haskell | removed host function and retained host type | applies | conforms with the published source-edit caveats — no removed record field is emitted; a representable retained associated type is natively deprecated, privately defaulted, optional, and type-only; incompatible type epochs omit affected equations/types | complete workflow — the native record renderer consumes only live sites; [`deprecated_host_type_compatibility`](../test-data/emissions/haskell/deprecated_host_type_compatibility/) observes the old equation's warning and compiles a current instance that omits it under `-Werror=deprecations` |

Normative policy lives in the shared facade-topology and deprecated-item
headings; each backend page references them and records only its host
realization. Shared preparation lives in `boundary_facade`; legacy emitters may
prove the same law through their existing descriptor and conversion paths.
The focused shared `retained_roots_omit_incompatible_same_name_newtype_epochs`
witness constructs two incompatible frozen payload declarations at one exact
identity and proves that both retained roots and their carrier/shell closure
are omitted before any backend can render an inexact compatibility surface.

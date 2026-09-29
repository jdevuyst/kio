# Backend emitters

Trigger: editing under `kio-rs/src/backends/`.

A backend emitter ("lowering") translates the post-typecheck IR into idiomatic source for one host language. This page is the map: where the emitters live, where emission sits in the pipeline, the runtime model a new emitter reproduces, and the disciplines that govern backend work. It does **not** restate the FFI contract — that's [`specs/backends/`](../../specs/backends/) — or the general kio-rs front-end — that's [`ai/topics/implementation.md`](implementation.md).

## Where the code lives

- [`kio-rs/src/backends/mod.rs`](../../kio-rs/src/backends/mod.rs) — the framework coordinator. Read its module docstring first: it defines the `Profile` descriptor (what the host language natively expresses — named records, sums-with-payload, pattern matching, GC, …), the `RuntimeSupport` convention (`None` vs `EmbeddedFile`), the **per-module `par_iter` fan-out** every `lower_package_*` entry point must follow (modules lower in parallel, concatenated in deterministic `BTreeMap`-order for byte-stable output), and the runtime-support-library convention.
- `kio-rs/src/backends/<lang>/` — one submodule per shipping backend (`js/`, `ts/`, `rust/`, `go/`, `swift/`, `haskell/`, `java/`, `python/`); the backend-neutral structural machinery they share sits beside them in [`skin.rs`](../../kio-rs/src/backends/skin.rs), [`structural.rs`](../../kio-rs/src/backends/structural.rs), [`reconstruct.rs`](../../kio-rs/src/backends/reconstruct.rs) (see § Runtime model), and [`namespace.rs`](../../kio-rs/src/backends/namespace.rs) (the `namespace` build-block key's per-backend default derivation and validation). Each backend ships a `profile()` plus `lower_*` entry points; two worked examples:
  - [`js/`](../../kio-rs/src/backends/js/) — `emit::lower_package_to_factory_module(&Package<Routed>, ns)` fans out per module, emitting a single branded `create<Handle>(host)` factory ES module (`<Handle>` = PascalCase of the artifact stem `ns`; the ts arm reuses this with the same `ns`). `RuntimeSupport::None`: dynamic typing makes every helper a one-liner the per-variant emit inlines.
  - [`rust/`](../../kio-rs/src/backends/rust/) — `emit::lower_package` / `lower_package_with_options(&Package<Routed>)` produces a `RustCrate` (file-name → content map). `runtime.rs` is the embedded `src/__kio_runtime.rs`; `thread_safety.rs` holds the send/sync classification. The body is **erased** behind opaque `KioStoredValue` tokens; the typed FFI skin consumes the prepared facade catalog to emit the sealed `KioType` marker algebra, exact nominal and application carriers, the host trait, and marker-directed conversions in `emit.rs`. `RuntimeSupport::EmbeddedFile` owns the raw default `Rc<dyn Any>` operations and their thread-safe `Arc<dyn Any + Send + Sync>` transform; public facade code never performs an unchecked downcast.
- [`kio-rs/src/backends/kio_prime.rs`](../../kio-rs/src/backends/kio_prime.rs) — the Kio' backend (`kio build` selects it via `kio_prime::emit_module` / `emit_package_file`): rebuilds `Module<Prime>` as surface and pretty-prints. Not an FFI backend — its emitted `.kio` is what `ci/infra/kio-prime-check-rs` verifies.
- [`kio-rs/src/cmd/build.rs`](../../kio-rs/src/cmd/build.rs) — the build-side dispatcher that selects a target's lowering and writes its files (including the embedded runtime-support file) under the build target's output directory.

A new backend plugs in as a sibling submodule: a `profile()` plus the `lower_*` entry points, wired into the build dispatcher.

## Where emission sits — the Kio' boundary

Emitters run at the **tail** of the pipeline (see [`kio-rs/src/pass/mod.rs`](../../kio-rs/src/pass/mod.rs) for the full phase list). The full front-end checks `Lowered`, substitutes its completion records into Prime, validates the assembled package with the standalone Prime checker, and canonicalizes its statement spine. The Kio' front-end reaches the same validated, canonical Prime boundary directly. The shared backend-agnostic preparation for host emitters is:

- [`pass::structural_recovery`](../../kio-rs/src/pass/structural_recovery.rs) plus [`pass::optimize`](../../kio-rs/src/pass/optimize.rs) — `Prime → Enriched` and backend-neutral optimization: collapses right-leaning intrinsic chains into the n-ary `Expr::Enriched*` structural nodes (records / tuples / projections / sums / match / conditional), then optimizes that enriched package. The combined result is the enriched-cache boundary.
- [`pass::recover_to_low`](../../kio-rs/src/pass/recover_to_low.rs) — `Enriched → Routed`: classifies every call site into an `Expr::Low*` variant (host fn, module fn, qualified module fn, closure, the newtype constructor / projector variants; indirect-call, one-binder `LowTypeApplication`, `__absurd__`), with mangled names pre-resolved and type-args pre-split from value-args. A direct call's adjacent leading binders may remain on that call's `type_args`; a standalone or computed type elimination is one nested `LowTypeApplication` per binder. Neither shape may cross a value-application boundary.
- [`pass::capabilities`](../../kio-rs/src/pass/capabilities.rs) — annotates the `Routed` IR (escape / lifetime annotations the Rust backend consumes).

The Kio' emitter consumes validated, canonical `Prime` directly. A host emitter consumes `Routed`, whose computations came from that strict Kio' and have been structurally recovered, optimized, routed, and annotated. Consequences:

- The surface-form boundary holds before the IR ever reaches an emitter — imported block calls such as `if!`/`else`, `scope!`, `do!`, and `match!`, tuple literals, other elaborator forms, and UFCS are all gone before Prime. Saturated, directly recoverable structural roots become `Enriched*` nodes. Residual applications — including partially applied and type-only application stages — and indirect uses remain generic through structural recovery and follow ordinary `Low*` routing. `__absurd__` has no structural form and receives its dedicated `LowAbsurdCall` route. An emitter never sees, and must never reintroduce, a surface-only form. (The headline rule is AGENTS.md § Universal rules — "Surface forms must not survive into Kio'"; the front-end side is [`ai/topics/surface-forms.md`](surface-forms.md), guarded by [`audit-surface-forms-survival`](../skills/audit-surface-forms-survival/SKILL.md).)
- An emitter dispatches over the pre-classified `Expr::Low*` / `Expr::Enriched*` families rather than re-deriving call-site kinds or structural shape. Don't re-walk for shape the recovery passes already established.
- `FnDef` / `FnExpr` canonical `Signature` groups are the sole authority for
  abstraction order. Type groups introduce one abstraction stage per binder;
  value groups introduce the ordinary product-domain function stages. Routed
  elimination reads only the explicit `type_args` / `LowTypeApplication`
  nodes already present in its tree. Never reconstruct either side from
  declaration parameter spelling, callee provenance, an ABI arity, or a
  producer-side side channel.

## Runtime model — the body and the FFI skin

A backend splits into two zones. The **FFI skin** is the host's typed contract — host record, host-native structural presentations, exported signatures, the per-signature boundary wrappers; it stays fully typed and idiomatic (see [`specs/backends/README.md`](../../specs/backends/README.md)). The **internal body** is the computation that implements the package's functions — no host ever reads it, calls into the middle of it, or sees its types.

**The FFI skin is a human-facing API, not merely compiler output that happens
to type-check.** A host author must be able to discover, name, construct, pass,
and inspect boundary values through ordinary host-language tools and idioms,
using the backend spec and host guide rather than reading generated internals.
Correctness, collision freedom, and deterministic naming are necessary but do
not discharge usability: a package-wide flat bucket of unrelated nominal
types, opaque codec/hash names as the ordinary spelling, mandatory public
`Any`/`Object` casts on a statically typed host, or helper scaffolding that
hides the real facade from documentation are findings when the host has a
clearer native organization. Preserve Kio's package/module/declaration
ownership in the host's natural namespace/type mechanisms, keep readable names
primary and collision-proof fallbacks exceptional, and compile representative
host-authored call sites against the actual public facade. The owning audit is
[`audit-backend-family-conformance`](../skills/audit-backend-family-conformance/SKILL.md)
§ Human-usable host facade.

Roles govern whether a literal is admitted and which literal capability it
uses; they never substitute a production host type. Runner fixtures may map an
exact host-type descriptor to a private representation key, but that key is
not the source annotation's type name and must not be derived from a role, a
leaf name, or the final segment of a qualified identity. Backend evidence must
keep same-leaf declarations from different modules distinct and exercise a
non-default exact host binding where the backend supports one; an enforced
backend default instead cites the comparable-language override evidence.

### Backend evidence boundary

Keep three evidence layers distinct:

- Cross-implementation language, diagnostic, runtime, and public-interface behavior belongs in `test-data/goldens/`. Golden-owned code treats generated host-backend files as opaque and hands their directory only to a fixed independent runner protocol; that runner is the default public-interface oracle.
- A backend-specific public-host fact that the runner cannot naturally and independently demonstrate, or a durable output-layout fact, belongs in one `test-data/emissions/<backend>/<case>/` case. `HOST_INTERFACE` compiles host source determined independently from the backend spec; a mechanically-wide host may be generated only from fixed checked-in parameters, never from emitted output. `ARTIFACT_SHAPE` pins only a specified fact or a portable proxy backed by a recorded measurement. Each case carries exactly one marker; there is no registry, and duplicating a runner-proven contract is a placement finding.
- Exact private helper spellings, allocator state, and other no-filesystem invariants belong in unit or mutation tests. Delete incidental private artifact assertions rather than preserving them in a corpus.

Kio' is the phase-artifact exception to the first rule: it is specified independently of any host backend, so a golden may read or assemble Kio' when that representation, its verifier/evaluator, or the dynamic-load boundary is the subject. Harness-owned phase comparisons remain valid.

Emission cases are supplementary facade/artifact evidence. They never satisfy a runtime `(shape, backend, direction)` cell in [`audit-backend-completeness`](../skills/audit-backend-completeness/SKILL.md); those cells still require a golden or POC that runs the value across the boundary.

## Comparative backend design

A backend is evidence about a semantic rule, not the scope of that rule. When
backend work establishes or revises a reusable design decision, classify the
decision at the narrowest truthful level:

- **Universal** — the rule follows from the Kio contract independently of host
  capabilities.
- **Semantic capability cohort** — the rule applies when a host, boundary
  occurrence, or emitted artifact has a stated capability; a cohort may cross
  language-family boundaries.
- **Family-common** — every current member independently satisfies the same
  predicate and realization law. Family membership is a summary only after
  that evidence agrees, not causal evidence by itself.
- **Backend-specific** — only host syntax, tooling, or integration mechanics
  differ. A reusable semantic law discovered in one emitter is not
  backend-specific merely because that emitter exposed it first.

This classification also governs factoring. Put shared semantic policy and
planning at the universal or capability-cohort level; use a family helper only
when all family members genuinely share its contract; keep rendering syntax
and unique integration leaves in the backend.

### Stable host release and capability floor

Backend admission, and any explicitly authorized re-baseline of an existing
backend, freezes a host-language release choice before an emitter mechanism is
selected. Choose the newest appropriate maintained GA/final stable release
line: the newest maintained stable LTS where that ecosystem has an LTS
convention, and the current stable line otherwise. A capability is admissible
design evidence only when it is part of that GA/final stable surface, is usable
through either the stable defaults or ordinary stable source pragmas,
manifest/edition declarations, or language-mode settings carried by the
emitted artifact or host build, works across the same package/module boundary
as the generated facade, and is available in every runtime or toolchain class
the backend contract admits. Preview, experimental, release-candidate,
nightly-only, unstable feature-gated, and experiment-only opt-in capabilities
such as `GOEXPERIMENT` do not qualify.

Keep the emitted artifact's concrete, ecosystem-native **language floor**
(language edition, standard revision, compiler series, or runtime line) separate
from the exact repository toolchain resolution. A directly mise-managed tool's
exact release is declared in the mise configuration and locked where that
backend supports locking. When mise manages an installer that delegates the
real compiler/runtime installation, the installer and delegated sub-toolchain
versions are both explicit in the mise configuration or its visible install
hook; do not claim the lock records a version it does not resolve. The floor is
a host compatibility contract justified by named stable capabilities and names
every ordinary stable source pragma, manifest/edition declaration, or
language-mode setting required to use them. The repository resolution makes
development and CI reproducible.

At admission or an explicitly authorized re-baseline, run the backend under
that exact repository resolution and separately prove the floor with either the
real floor toolchain or a compiler mode that truthfully enforces it. The floor
evidence compiles both the emitted artifact and the complete host-language fence
set with exactly the named non-default stable pragmas, declarations, and modes
in force, and with no preview or unstable experiment enabled. Merely passing
under a newer compiler without enforcing that floor and those modes proves
neither. Once accepted, the floor is fixed; moving it is an explicit
compatibility change with its own authority, migration analysis, specs, docs,
and evidence. Exact repository-resolution updates remain the reason-driven
domain of
[`upgrade-deps`](../skills/upgrade-deps/SKILL.md).

This admission rule does not automatically re-baseline an existing frozen
floor or claim that a separate historical floor-toolchain lane already exists.
Audits still report a direct contradiction between an emitted feature and the
declared floor, but they do not invent a new legacy lane or floor change without
the authority that would make that work in scope.

Release choice and facade fidelity have different semantic scopes. Release
selection is universal backend governance; concrete version syntax and
toolchain integration are backend-specific. Preserving a public relationship
is a semantic capability rule: when the selected stable host, under the named
stable artifact/build modes, can express that relationship at a boundary
occurrence across every runtime/toolchain class the backend contract admits, a
private erased body, the discovery backend's family, or compatibility with an
older release cannot justify replacing it in the public facade with `Any`,
`Object`, `any`, unchecked casts, or a less usable generated shape. Establish
each occurrence independently and place shared planning at that capability
level rather than at a language-family label.

### Decision record and propagation gate

Authority comes first. An authorized host-observable law belongs in the
normative specs at its semantic scope. A contract-preserving implementation
invariant belongs in shared code, tests, and the owning engineering topic. A
proposal or ambiguity still requires user authority before implementation.
Emitter behavior, tests, audit findings, and similarities among peers are
evidence; none can create or broaden a behavioral contract, and discovering a
shared defect does not authorize fixing unrelated occurrences.

For each settled reusable backend decision, preserve a durable record in its
owning tracked artifacts. Record the authority, backend-neutral law, rationale
and rejected alternatives, semantic applicability predicate, open-world and
evaluation consequences, evidence, and every normative, implementation, test,
guidance, and audit destination. A scratchpad may stage that work but is not
the durable record.

Build the propagation ledger by enumerating every shipping backend and every
semantically distinct occurrence to which the predicate may apply. The current
backend-completeness shape matrix is a starting inventory, not permission to
skip a distinct occurrence. Split an occurrence only when the rule can observe
the difference, and do not manufacture impossible host/shape cross-products.
For each row record three independent results:

- **Applicability:** `applies`, `not applicable — <failed predicate>`, or
  `unresolved`.
- **Conformance:** `conforms`, `change required`, `not applicable`, or
  `unresolved`.
- **Evidence:** `unassessed`, `mechanism fact`, `predicate proof`, `partial
  workflow`, or `complete workflow`.

An applicable row closes only as `applies / conforms / complete workflow`; a
non-applicable row closes only as `not applicable / not applicable / predicate
proof`. Grouping rows by family is valid only after every member reaches the
same result. Public specs state only implemented current behavior, so a
normative shared law lands with its conforming implementations and evidence;
rationale, proposals, and incomplete rows remain in their engineering or
tracking homes. An owned incremental residual does not close the overall
propagation gate.

Every new backend repeats this gate against every universal rule and every
shared rule whose semantic predicate applies. It references the shared
normative heading (including any `**Applies when:**` clause) and records only
its host realization on the per-backend page; it does not copy the general law
or state behavior its implementation does not provide.

The skin's **backend-neutral structural conventions** have one code home each. Prepared boundary topology and live execution provenance live in [`crate::backends::boundary_facade`](../../kio-rs/src/backends/boundary_facade.rs); the legacy type-walking profiles and pure structural helpers live in [`crate::backends::skin`](../../kio-rs/src/backends/skin.rs) and its neighbours. Participating emitters consume the narrow planning surface, profile trait, or pure helper their independently classified representation requires; a backend-specific realization remains explicit where the host's representation calls for one:

- The **right-spine walk** over `&` / `|` is a pure `Type` operation — [`Type::right_spine_product`](../../kio-rs/src/ast.rs) / `Type::right_spine_sum`; emitters call it directly (Rust's `right_spine_walk_*` collect the borrowed walk into owned slots for its private body conversion).
- The **3-step key fallback** applies when the host facade exposes keyed
  fields, cases, patterns, or presentation helpers. Prepared facade topology
  selects its bare / qualified / positional `SemanticKey` once; an applicable
  backend renders that exact key against its identifier or object-key syntax.
  A canonical positional carrier such as Rust's binary `Product` / `Sum`
  consumes the prepared payload topology and site identity but has no keyed
  presentation to render. A public emitter never repeats the candidate walk or
  resolves a bare leaf from ambient package contents. The pure `skin` helpers
  remain available only to non-public/internal conversion code whose input is
  not a prepared facade occurrence.
- The **per-signature boundary-wrapper driver** is `skin::convert` over the `skin::SkinProfile` trait: it walks a signature's type, short-circuits passthrough leaves, dispatches on the type constructor, and flips the conversion direction (`skin::FfiDir::flip`) across a function value's parameter leg. The walk is shared; the *leaves* are the profile's hooks (`is_passthrough`, `convert_newtype`, `convert_product`, `convert_sum`, `convert_function`), each holding the backend's private value representation and recursing back through the driver. Its remaining production profiles are JS's `JsSkin` and Haskell's `HaskellSkin`; public sites on every backend consume the prepared facade first, while Rust, Go, Java, Python, and Swift use backend-specific prepared-plan converters. Rust retains the pure `right_spine_walk_*` helpers for its private marker-directed conversion. TypeScript reuses JavaScript runtime emission.
- The **prepared boundary facade** is the sole public shape catalog. It records semantic uses, generic structural shell identities, exact nominal dependencies, declaration-head stages, paired live execution layouts, and retained source-compatibility provenance once. Every backend consumes the applicable facts for its public host contract and keeps only host syntax and private erased-body conversion leaves locally; a backend without keyed presentation need not consume the shell's semantic-key identity. A retained site keeps frozen semantic declarations but cannot acquire package execution authority; a backend may render only an explicitly deprecated, genuinely optional compatibility shim when its removal policy requires one.
- Rust's marker facade follows Kio's binary product/sum tree compositionally:
  `KioProduct<A,T>::Facade = Product<A::Facade,T::Facade>`, so substituting
  `T := KioProduct<B,C>` produces
  `Product<A::Facade,Product<B::Facade,C::Facade>>`. Haskell's native fold has
  the analogous right-nested equation. These are backend realizations, not a
  family-wide choice between flat and recursive public structural carriers;
  each backend's specification owns that representation decision.
- The Haskell emitter gives a parametric source newtype one **declaration-stable nominal head** wherever it represents the newtype as typed host code. Saturation, facade-vs-native-body reachability, and payload classification (ordinary function versus dictionary-shaped function included) must not choose a different identity on those typed paths. Haskell keeps only nullary, nonrecursive, nonexistential newtypes transparent there; parametric, recursive, and existential declarations remain nominal, with public construction and projection mediated by the source members' wrappers. A private universal fallback may still carry the value in its erased body representation; that carrier is not another typed newtype head and does not change fallback eligibility.
- The **`host_descriptor`** contract shape lives in [`crate::host_descriptor`](../../kio-rs/src/host_descriptor.rs); `render_js_host_factory_prelude_names` and `render_rust_host_trait` render it in their own surface syntax.
- **Opaque host-type bindings are declaration-keyed but non-injective.**
  The exact module path and declaration name select the descriptor entry and,
  where exposed, the host binding entry; they do not require different host
  representations. Any compatible entries may select the same concrete host
  type or type constructor. Never synthesize a wrapper, brand, tag, defined
  type, `NewType`, or marker solely to mirror distinct opaque Kio identities
  in the host language. Such nominal machinery is appropriate only when
  required by an explicit Kio `newtype` or by an independently justified
  target representation. An independently authorized declaration-scoped
  adapter for a relationship the host cannot state is assessed under its own
  narrow scope and failure or unsafe invariant; this paragraph neither
  authorizes nor claims that such an adapter exists.
- **Prefer host-selectable bindings, then overridable defaults.** A fixed
  backend mapping is the fallback only after comparing similar host languages
  and existing Kio backends and establishing that the target cannot expose a
  truthful, reasonably usable configurable surface. Record that comparison
  and decision in the emitter's module documentation, and state the
  host-visible policy in the backend spec and host guide. A backend may publish
  defaults or a fixed table organized by `role(...)`, but that table is an
  explicit backend FFI policy: the Kio role itself still admits syntax and
  never semantically chooses or infers the host representation.

#### Facade topology and execution provenance decision record

The authority is the pre-existing public contract in
[`specs/backends/README.md`](../../specs/backends/README.md): function-type FFI
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
[`ffi_host_poly_function_newtype_roundtrip`](../../test-data/goldens/00_success/ffi_host_poly_function_newtype_roundtrip/)
executes a host callback, a public polymorphic-newtype value in both directions,
and a package export, while
[`ffi_newtype_visibility_facade`](../../test-data/goldens/00_success/ffi_newtype_visibility_facade/)
exercises host-facing public-newtype-member sites across the visibility
combinations.

| Backend | Occurrence | Applicability | Conformance | Evidence |
| --- | --- | --- | --- | --- |
| JS | live callable sites | applies | conforms — current declarations build the facade and host dispatch | complete workflow — both shared all-backend facade workflows |
| JS | removed host function | not applicable — no frozen declaration is preserved | not applicable — incompatible retained epochs likewise create no generated JS surface | predicate proof — current-only host scan; [`deprecated_host_fn_removal`](../../test-data/emissions/js/deprecated_host_fn_removal/) asserts the removed property is absent and the live property remains |
| TypeScript | live callable sites | applies | conforms — the `.d.ts` and shared JS runtime use the current interface | complete workflow — both shared all-backend workflows through the typed facade |
| TypeScript | retained removed host function | applies | conforms with the published incompatible-epoch source-edit caveat — the `.d.ts` keeps each representable exact callable as an optional deprecated property with a visibly deprecated transitive type closure; incompatible same-identity epochs are omitted, while the shared JS runtime stays current-only | complete workflow — [`deprecated_host_fn_source_compatibility`](../../test-data/emissions/ts/deprecated_host_fn_source_compatibility/) type-checks unchanged and current-only hosts under `--strict`; [`deprecated_host_fn_shape`](../../test-data/emissions/ts/deprecated_host_fn_shape/) checks markers, optionality, transitive support, and runtime inertness; the shared incompatible-epoch witness checks complete root and closure omission |
| Python | live callable sites | applies | conforms — current declarations build namespace entries and validation | complete workflow — both shared all-backend facade workflows |
| Python | history-only host declarations | applies | conforms — Python 3.10 stdlib cannot mark a retained stub declaration deprecated, so both the `.pyi` stub package and `.py` omit the complete history-only closure and current-host validation remains live-only | complete causal workflow — the mixed live/retained unit rejects leaked roots, adapters, carriers, and generic propagation; the sealed-history build test proves a valid history shares the live-only cache while malformed history is still rejected; [`deprecated_host_fn_shape`](../../test-data/emissions/python/deprecated_host_fn_shape/) checks both artifacts and [`deprecated_host_fn_live_only`](../../test-data/emissions/python/deprecated_host_fn_live_only/) executes a current host |
| Java | live callable sites | applies | conforms — live descriptor methods build the interpreter adapter | complete workflow — both shared all-backend facade workflows |
| Java | retained removed host function and dependencies | applies | conforms with the published source-edit caveats — deprecated throwing defaults and each emitted deprecated transitive dependency remain source-only; removed host roots leave package generics, dependent retained signatures use a deprecated declaration-owned carrier, incompatible epochs omit affected methods/types, and the interpreter adapter stays live-only | complete workflow — focused units cover support, live dominance, deprecated transitive members and current-host optionality; [`deprecated_host_fn_source_compatibility`](../../test-data/emissions/java/deprecated_host_fn_source_compatibility/) compiles unchanged and live-only external hosts and runs both through the live export |
| Rust | live callable sites | applies | conforms — live descriptor methods supply package host calls | complete workflow — both shared all-backend facade workflows |
| Rust | retained removed host function and dependencies | applies | conforms when the frozen signature is nameable — the trait default and transitive support are deprecated and inert; a removed associated host type, an incompatible declaration epoch, and every dependent retained surface are omitted under the published source-edit caveats | complete workflow — [`deprecated_host_fn_shape`](../../test-data/emissions/rust/deprecated_host_fn_shape/) checks the durable declaration and markers; [`deprecated_host_fn_source_compatibility`](../../test-data/emissions/rust/deprecated_host_fn_source_compatibility/) compiles unchanged and live-only hosts and runs both through the live export; focused units check live dominance and dependent omission |
| Go | live callable sites | applies | conforms — the live origin owns bounded converter capabilities | complete workflow — both shared all-backend facade workflows; [`facade_generic_shell_reuse`](../../test-data/emissions/go/facade_generic_shell_reuse/) compiles and runs the same host against baseline and unrelated-declaration builds |
| Go | retained removed host function and dependencies | applies | conforms with the published source-edit caveats — every representable retained-only alias, shell, carrier, and constructor has a standard deprecation directive and exposes no live capability; removed host roots leave package generics, and incompatible epochs omit affected roots/types | complete workflow — [`deprecated_host_fn_source_compatibility`](../../test-data/emissions/go/deprecated_host_fn_source_compatibility/) compiles unchanged and live-only hosts, runs both through the live export, and checks retained aliases; [`deprecated_host_fn_shape`](../../test-data/emissions/go/deprecated_host_fn_shape/) checks markers and absence of the removed method; focused units check live dominance and current-host selection |
| Swift | live callable sites | applies | conforms — the current protocol supplies package dispatch | complete workflow — both shared all-backend facade workflows |
| Swift | retained removed host function and dependencies | applies | conforms with the published incompatible-epoch source-edit caveat — every representable protocol requirement has a deprecated trapping default, removed host types have deprecated concrete defaults, every emitted transitive declaration is deprecated, incompatible epochs omit affected roots/types, and adapters/dispatch remain live-only | complete workflow — focused emitter units and [`deprecated_host_fn_type_compatibility`](../../test-data/emissions/swift/deprecated_host_fn_type_compatibility/) check old/current host compilation, markers, defaults, live dominance, and runtime inertness |
| Haskell | live callable sites | applies | conforms — the host record and both body paths use live functions only | complete workflow — both shared all-backend facade workflows |
| Haskell | removed host function and retained host type | applies | conforms with the published source-edit caveats — no removed record field is emitted; a representable retained associated type is natively deprecated, privately defaulted, optional, and type-only; incompatible type epochs omit affected equations/types | complete workflow — both record renderers consume only live sites; [`deprecated_host_type_compatibility`](../../test-data/emissions/haskell/deprecated_host_type_compatibility/) observes the old equation's warning and compiles a current instance that omits it under `-Werror=deprecations` |

Normative policy lives in the shared facade-topology and deprecated-item
headings; each backend page references them and records only its host
realization. Shared preparation lives in `boundary_facade`; legacy emitters may
prove the same law through their existing descriptor and conversion paths.
The focused shared `retained_roots_omit_incompatible_same_name_newtype_epochs`
witness constructs two incompatible frozen payload declarations at one exact
identity and proves that both retained roots and their carrier/shell closure
are omitted before any backend can render an inexact compatibility surface.

### Retained host declarations

Classify each emitted host-visible declaration and dependency by provenance.
It is **live** when the current bridge-selected surface reaches it, and
**history-only** only when its sole provenance is a removed declaration
recovered from sealed `*.sig.kio` history. Provenance joins per declaration:
live dominates history-only. A declaration reached by both remains live and
must not be deprecated merely because frozen history also reaches it.

One exact nominal identity also cannot merge incompatible declaration epochs.
The live declaration wins over an incompatible retained snapshot and every
retained root that depends on that snapshot is omitted; if two retained epochs
conflict, every root depending on that identity is omitted. Never union,
intersect, widen, or choose one epoch by encounter order to keep such a root
nameable. The normative rule and per-backend source impacts live in
[`specs/backends/README.md` § Incompatible retained declaration epochs](../../specs/backends/README.md#incompatible-retained-declaration-epochs).

History-only material is source-compatibility facade only:

- Every host-visible history-only declaration — the removed root and every
  history-only alias, nominal or structural type, constructor, helper, or
  other transitive facade dependency — carries the target's recognized
  deprecation marker.
- A host written against the current package implements, provides, and selects
  only live items. A retained callable is optional either through genuine
  structural omission or a real default. A retained associated type, type
  binding, facade type argument, or other type choice is optional through a
  real default or imposes no selection at all. Retaining old generic arity by
  forcing a new host to choose a placeholder type is not optional.
- History-only material contributes nothing to loader requirements or
  matching, current-host completeness or validation, live host adapters,
  exports, package-handle capabilities, executable package routing, or live
  facade execution/conversion plans. A retained callable may exist only as an
  explicitly deprecated, genuinely omittable structural member or as an
  unconditional trapping compatibility default. Host source can name and
  directly call a supplied structural member or the trapping stub, but neither
  is package functionality and no package path can invoke it.
- If the target cannot keep an old spelling available while making it both
  omittable by a new host and visibly deprecated, omit it. Document the
  resulting host-source compatibility break through the ordinary concrete
  caveat banner and emitter mutual-citation rules; never preserve it by making
  the removed item mandatory again.

The **generic structural-shell identity** is backend-neutral
([`specs/backends/README.md` § Generic structural shell identity](../../specs/backends/README.md#generic-structural-shell-identity)):
`FacadeShellId` contains only product/sum kind plus ordered semantic keys;
payload types remain generic arguments at each use. Backends either render the
shared reversible codec, apply a fixed target-path bounded spelling to that
exact identity with collision rejection, or use an unnamed native structural
form. None may derive public names from payload types or registry occupancy.
The per-signature boundary-wrapper *leaves* that resist sharing — a product as
a JavaScript object versus a Rust struct, private binary-versus-flat body
conversion, and per-backend sum values — remain each emitter's, per the shared
spec's § Per-backend obligations.

Haskell has three deliberately separate naming surfaces. Exact host-type
families, host fields, and exports use readable `__`-separated source
components only when every component contains no `__` and neither starts nor
ends with `_`; that boundary-safe grammar is reversible. Other identities use
role-specific deterministic fallbacks: host fields and exports use the
versioned typed codec, while exact host-type families use the raw
`module\0leaf` hexadecimal form. The structural namespace escape applies when
an exact host-type fallback overlaps a fixed facade claim.
`KioCarrier_H…` and `KioExistential_H…` remain declaration-local nominal names;
fixed facade names derive from the package namespace; and
[`haskell/naming.rs`](../../kio-rs/src/backends/haskell/naming.rs) owns the
primary-name grammar, the distinct exact-host fallback, and the versioned typed
codec for internal members, boundaries, patterns, selectors, and rank helpers.
The independent Haskell runner codec in
[`haskell/abi.rs`](../../ci/infra/kio-test-runner-rs/src/haskell/abi.rs)
reconstructs only that public semantic identity from source declarations and
fixed test vectors. Neither side parses the bounded readable suffix. The
normative byte framing and namespace prefixes live in
[`specs/backends/haskell.md` § Item naming](../../specs/backends/haskell.md#item-naming).

The body targets one **universal erased value model**, the conformant floor every emitter reproduces:

- `atomic` (host-primitive passthrough) · `closure` (n-ary) · `sum` (tag + payload) · `product` (slot vector) · `unit` · `host-ref` · `erased-poly` (an erased reference type, e.g. `Rc<dyn Any>`).

Type representations and type-directed dispatch drop in the body: a
well-typed Kio' body is parametric, so it runs uniformly over these shapes.
The **application tree does not drop**. Each canonical `Forall` binder is one
semantic abstraction/application boundary, and Routed records each
elimination either in a direct call's ordered `type_args` or as one nested
`LowTypeApplication`. For each direct, escaped, or computed occurrence, the
emitter preserves every semantic boundary, its order, and its evaluation
effects under the current applicable contract. Host type abstractions,
callable stages, erased wrappers, and compaction are occurrence-specific
realizations; family membership and body erasure never select among them.
Computation stays at the exact boundary that exposes it and cannot cross a
later type or value application. The facade follows the current shared and
per-backend contracts. **JS** realizes the erased value floor directly — sums
`[tag, payload]`, products `[x, y]`, unit `null`, newtypes share their payload's
shape, lift / peel are runtime identities (see the
[`js/emit.rs`](../../kio-rs/src/backends/js/emit.rs) header). Erasure applies
only to the body representation, **never** to the skin or evaluation order.

When an occurrence uses native host type abstraction, audit its direct and
first-class realization independently and record it in the decision ledger;
do not infer a family-wide callable ABI from that mechanism. Boundary adapters
retain the documented facade and evaluation behavior for that backend.

A higher-kinded carrier `F(A)` adds no HKT-specific representation to an
**erased body**. JS realizes that fact directly. Static members of the family
also use one private universal representation, but that does not make an
arbitrary native facade application equal to the universal value and does not
remove the typed boundary conversion. Each public skin must state the exact
constructor/application relation it can actually enforce.

Rust does so with the sealed `KioType` marker algebra: a kind-`*` binder is a
marker `A`, a value occurrence is `A::Facade`, abstract application is the
marker `KioAppliedN<F, A, ...>` whose facade is `KioApplyN<F, A, ...>`, and
the selected `KioTypeConstructorN` witness supplies the public `lift` /
`project` seam. Generated declaration-owned witnesses name exact nominal
applications; external witnesses may compose the same algebra by selecting
any public, well-formed `KioType` marker. `Apply` remains the exact public-
facade authority. Defaults use the selected marker codec; an external witness
may override both methods with a lossless, substitution-stable opaque-token
representation. Neither choice changes the abstract `KioAppliedN` /
`KioApplyN` carrier. `KioStoredValue` stays opaque and raw construction/
downcast stays crate-private. `KioValue<A>` performs typed pack/unpack for
container storage. This means Rust performs no HKT-specific *body walk*, but
it does perform the selected-marker default or a lawful constructor override
at the typed boundary; never describe that as an abstract native `F::Apply`
value skin or generic scalar downcast. Go, Swift, and Java keep their separately
documented public carrier mechanisms; a Rust proof is not family-wide
realization evidence.
Allocation, cloning, performance, debugging, and layout effects remain
separate observable questions and require their own evidence.

The `RuntimeSupport::EmbeddedFile` convention names shared body fragments once in an emitter-written file rather than inlining them at every call site. Rust uses it for the private storage operations behind `KioStoredValue`; generated `KioType` conversions are the typed seam and raw erased-token construction/downcast remains crate-private (default `Rc`, thread-safe `Arc + Send + Sync`). Go's erased body uses the same convention to name the canonical `Unit` (`struct{}` — Go has no built-in unit value) once in `kio_runtime.go`.

Go's erased body is statement-oriented. `go/emit.rs` lowers into a `GoBlock` plus an explicit destination (`return`, assignment, or discard), materializing non-trivial children into deterministic per-function locals in source order. `let`, conditional, and match therefore become ordinary Go statements instead of nested call-once closures that simulate control flow; Kio function values and bounded helper or typed-skin conversions may still require closures. This shape is load-bearing: rebuilding expression-valued control flow from call-once closures makes nested Kio terms into nested Go inliner inputs. [`wide_label_statement_lowering`](../../test-data/emissions/go/wide_label_statement_lowering/) pins the recorded Go artifact proxies — bounded call-once IIFEs and generated-line length — while the typed FFI skin remains unchanged.

## Package namespace

Every name an emitted artifact contributes to a host program — the package handle, the factory, the host type, structural facade symbols, the runtime-support items — is either the per-package namespace itself or is scoped beneath it; **no emitter contributes an unscoped package-independent identity**. Fixed member spellings inside a package / module / crate remain compatible with this rule because the namespace qualifies them. The contract is [`specs/backends/README.md`](../../specs/backends/README.md) § The package facade; the implementation has one home per concern:

- A public facade path preserves its semantic identity before target-language
  rendering: exact source module components and their boundaries, plus whether
  each selector denotes a nested module, type, or value. Never flatten those
  distinctions into one host namespace and then repair collisions with
  declaration-order suffixes. The renderer must be deterministic and
  injective under the host language's escaping and name-equivalence rules, so
  adding an unrelated declaration cannot rename an existing public selector.
  Prefer readable host idioms on the injective subset, but keep the fallback
  usable by a human host author. Runtime host bindings likewise retain exact
  `(module path, leaf)` identity until the backend renders the published host
  key; neither emitters nor runners recover ownership by splitting a flattened
  string.
- [`kio-rs/src/backends/namespace.rs`](../../kio-rs/src/backends/namespace.rs) owns each backend's `default_<lang>_namespace` / `validate_<lang>_namespace` pair, the reserved-word tables, and the **unimportable-name** mangles — names that are legal identifiers but unloadable in the host (Go's `main` / `internal`, Haskell's `Main` / `Prelude`, Swift's `Swift` / `Foundation`); a new backend asks the same question for its language. `pascal_case` derives the brand: handle = PascalCase of the namespace's final segment, host contract = `<Handle>Host`, factory = `create_<handle>` in the language's casing.
- The `namespace` build-block key is recognized by every backend arm in [`kio-rs/src/cmd/build.rs`](../../kio-rs/src/cmd/build.rs) (duplicate check, the backend's validation, default from the package name — the go arm is the template a new backend copies).
- Runtime-support content is a function of emitter version, documented
  representation-affecting target options, and namespace binding: the support
  file's namespace-bearing header line is substituted per package, everything
  below it stays byte-identical across packages built with those same options,
  and the file lives inside the package's namespace
  (`specs/backends/README.md` § Runtime-support library).
- The corpus harness supplies each test runner with the Kio source-package
  name and the current target's effective namespace override. The package name
  is only the input to the backend's public default derivation when no
  override exists; backend artifact identity is the resulting effective
  namespace. The runner never
  parses emitted source, a manifest, marker, header, or artifact stem to
  rediscover that input. These arguments identify the artifact; they do not
  carry semantic host-interface data.
  [`audit-runner-host-fidelity`](../skills/audit-runner-host-fidelity/SKILL.md)
  § 6 sweeps this boundary and hardcoded-name failures.

## Disciplines (canonical homes — don't restate, follow)

Backend work is governed by AGENTS.md § Universal rules and the audit suite. Before editing an emitter, know which rule bites:

- **Per-backend limitations: only genuine impossibilities, and mutual-cited.** A carve-out is admissible **only** when the host language fundamentally cannot express the spec — never as a way to defer fixable work. Almost no Kio feature is genuinely impossible: see the escape-hatch section below. When a shipping backend has such a caveat, it appears in **three** places — matching concrete-caveat banners in `specs/backends/<lang>.md` and `docs/hosts/<lang>.md`, plus the emitter code comment naming the detailed spec section. A new backend with a known caveat stops normal `add-backend` acceptance for a user decision instead of publishing a maturity label. Its required `Host API stability: evolving` field records compatibility policy and supplies no caveat or acceptance relief. AGENTS.md § Universal rules; swept by [`audit-spec-drift`](../skills/audit-spec-drift/SKILL.md) § 7 (both the citations *and* the per-carve-out discharge).
- **Semantic applicability governs propagation.** Classify reusable backend decisions, enumerate every applicable backend and distinct occurrence, and close the applicability/conformance/evidence ledger before claiming completion. § Decision record and propagation gate; swept by [`audit-backend-family-conformance`](../skills/audit-backend-family-conformance/SKILL.md) § Cross-backend decision propagation.
- **Family conformance.** A per-backend page departing from its declared family's shared idioms carries an explicit mutual-cited divergence in host realization while still implementing the full Kio contract. A family divergence cannot excuse an unsupported or degraded feature or serve as a per-backend limitation. [`specs/backends/README.md`](../../specs/backends/README.md) § Language families; swept by [`audit-backend-family-conformance`](../skills/audit-backend-family-conformance/SKILL.md). The per-backend page also follows the 9-section structure — [`audit-backends-shape`](../skills/audit-backends-shape/SKILL.md).
- **Spec-has-impl-doesn't is always a finding.** The emitter implements what the page documents. Swept by [`audit-spec-drift`](../skills/audit-spec-drift/SKILL.md) § 7.
- **Bugs surface; goldens demonstrate behavior, not workarounds.** When a backend can't build / run a golden the spec admits, fix the emitter — don't omit `target=<lang>` from the `build { ... }` block, don't add a `|| exit 0`-style guard, don't reshape the `.kio` source (extra annotations, monomorphization, alias scaffolding) to dodge an emitter gap. Failing goldens stay in place. AGENTS.md § Universal rules; swept by [`audit-corpus`](../skills/audit-corpus/SKILL.md) §§ 1–3.
- **No partial implementations.** A closed issue is fully fixed; a genuine slice is labeled and leaves the tracker open. AGENTS.md § Universal rules; swept by [`audit-partial-implementations`](../skills/audit-partial-implementations/SKILL.md) — § 6 is the emitter-specific one (a guard that narrows on a property of a spec-admitted shape and silently identity-passes the complement is a finding, not a limitation).
- **Generated bindings are outside the user-reachable namespace.** Package/module helpers, locals and parameters, backend type parameters, generated aliases or nominal owners, and identities introduced by imports follow [`ai/topics/implementation.md`](implementation.md) § Generated binding hygiene. Reserve an identity class outside legal Kio mappings where possible; otherwise allocate private implementation bindings from the complete seed for the actual host value, type, module, package, or other namespace and render every declaration and use from the same identity. Classify imports into the namespace they really populate; do not presume an independent import namespace. Public facade identities retain the stable injective semantic mapping above and never use occupancy-dependent renaming. Property labels and structural-shape keys used only for lookup are not bindings. Compare identities under the host's post-render equivalence, not raw strings; a counter or conventional prefix alone is not a proof. [`audit-generated-binder-hygiene`](../skills/audit-generated-binder-hygiene/SKILL.md) sweeps compiler and backend binding sites.
- **IR family bias (informational).** An IR variant exercised by only one language family may have baked-in assumptions; [`audit-ir-family-coverage`](../skills/audit-ir-family-coverage/SKILL.md) reports per-family coverage so the shape can be refactored before more backends calcify it.

The `allow(dead_code)` carve-out for *emitted* package source (runtime helpers an emitted package may not exercise) is the one exception to the no-dead-code rule — see [`ai/topics/implementation.md`](implementation.md) § No `allow(dead_code)`.

## Generated-source compiler resource failures

Backend design reviews performance before a resource failure appears. Check
the asymptotic shape of package preparation and emission, whether one semantic
transaction is recomputed for sibling artifacts or selected targets, generated
source bytes/maximum line/nesting on a natural wide package, and wall time plus
peak RSS of the real host compiler. Compare a narrow control so fixed startup
cost is not mistaken for scaling. A portable structural assertion pins the
load-bearing bounded shape; machine-specific wall/RSS thresholds do not replace
that causal guard. This review is proportional and focused—ordinary changes do
not require a broad benchmark when they cannot affect these axes.

An OOM, swap storm, or extreme compile time in a host toolchain is codegen evidence before it is a request for a larger resource allowance. Isolate one generated compilation unit and record its wall time and peak RSS with the real host compiler; compare it with idiomatic host source of similar size, and inspect structural metrics such as line length, closure/IIFE count, and nesting depth. Compiler phase switches may attribute the cost, but they are diagnostic unless downstream hosts can use them without changing the delivered program's performance contract.

Concurrency gates, job caps, and scratch-directory redirects can keep a harness reliable while diagnosis proceeds. They do not establish that the emitted unit has a reasonable footprint. A resource-control change therefore states whether it is mitigation or root-cause correction; closing the issue requires a codegen-shape fix, a repeated per-unit measurement, and a portable regression proxy when peak-RSS gating itself would be machine-dependent. Reassess temporary caps after the corrected shape is measured.

## The escape hatch — completeness through the backend's last resort

A carve-out is "genuinely impossible" only when the host language has **no escape hatch**. Most host languages have one: Rust has `unsafe` and unchecked coercions; C / Go / Swift / Kotlin have FFI, casts, and `unsafe`-equivalents; dynamic backends erase types and sidestep the problem entirely. Where an escape hatch exists, almost no Kio feature is genuinely impossible to emit completely — the erased `Rc<dyn Any>` body *is* Rust's safe expression of erasure, and reaching for `unsafe` would be a regression from it.

So the default for any per-backend shortcoming is **fix it with the safe, idiomatic construct**. Prefer that solution *very strongly*. The escape hatch is a **true last resort**:

- Exhaust the safe / idiomatic path first (a richer IR shape, a better codegen template, an additional runtime helper). Erasing the whole body to the open universal (`Rc<dyn Any>`) carried HKT on a strict static host with no `unsafe` — the lesson is that the apparent wall is usually a representation choice or a missing helper, not a language limit.
- Any use of the escape hatch **must carry an informal proof of correctness** at the site — why the `unsafe` / cast / FFI shim upholds the invariant the safe path would have enforced (the erased value's true type, the layout assumption, the lifetime). An escape-hatch use without that proof is a finding, not a workaround.

A feature is genuinely impossible to emit completely **only** when the host language has no escape hatch at all — e.g. Elm, a pure language with no FFI and no `unsafe`. There, and essentially only there, a three-site caveat is the honest outcome. Everywhere else, "the safe path is hard" is not "impossible," and the burden of proof for documenting a limitation (per AGENTS.md § Universal rules) is correspondingly high.

## Boundaries

- **vs [`specs/backends/`](../../specs/backends/)** — that's the *contract* a host calls against: output layout, loading protocol, package API, host-record mapping, FFI type-mapping, the prepared right-spine walk / 3-step semantic keys / generic shell identity, and item naming. The spec says *what* the boundary must look like; this page and the emitter code are *how* a lowering achieves it. The 9-section per-backend page structure and the cross-cutting README live under [`ai/topics/specs.md`](specs.md) § What goes into a backend spec — don't duplicate any of that here. **Codegen internals that don't cross the FFI boundary stay out of the spec by rule.**
- **vs [`ai/topics/implementation.md`](implementation.md)** — that's the general kio-rs crate: the two binaries, the phase-polymorphic typer, feature gating, standard Rust tooling, the no-dead-code rule. Read it for everything before the `backends/` tail.

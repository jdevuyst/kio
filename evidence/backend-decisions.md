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

## Guest-contained tail transfers (Stage 1)

This decision implements the [generated-code tail-call law](../specs/backends/README.md#tail-calls-within-generated-code).
Its predicate is an executing final value or type application to a Kio callable
within one generated artifact, including calls between its package instances.
It is universal across the eight host backends, not an erased-body family rule.
The public callable types, application stages and host interfaces are unchanged.
This record contains source arguments and bounded observations, not a completed
Stage 1 acceptance claim or a general throughput claim.

### Source argument and representation choice

The shared [`TailContext`](../kio-rs/src/backends/tail_plan.rs) borrows checked,
routed expressions. Its `ResultChild` selects the actual let/sequence body,
conditional branch or match arm that supplies an invocation's result; it cannot
attach that context to an arbitrary child. Operands remain non-tail, and a
function body starts a fresh context when invoked. `TailStage` distinguishes the
last executing type application from a value application, including a type
stage whose result is itself callable. Known host calls are ordinary leaves.
The plan adds no persistent Prime syntax, node identities or producer authority.

JS, Rust, Go and Swift consume that plan during emission; Python and Java
consume its markers through the shared [serialized runtime IR](../kio-rs/src/backends/serialized_runtime_ir.rs).
Haskell retains native tail application and strict stage sequencing rather than
adopting an erased dispatcher. That realization still owes the same law; native
tail position alone is not evidence about arbitrary host-selected monads.

The private representation keeps the actual guest entry through generic,
structural, nominal and existential transport. In Rust and Go, a first-class
value stage consumes one canonical domain: Unit, one value, or a right-nested
product. Source binders determine unpacking, not an extra callable-result
adapter; direct named entry layouts and public native parameter groups remain
separate. Executing type stages stay distinct from Unit-domain value calls.
Swift keeps private callable and structural values with lexical-witness-owned
native-view factories; constructing a public native view occurs at its actual
boundary, not around a private transfer. No opaque host container is traversed.

JS recognizes artifact-owned callable identities in a `WeakMap`; Python uses
exact private-class identity; Java rebinds its interpreter to the guest entry's
runtime, body, environment and stage. Rust/Go step loops and the JS/Swift
invocation-local cursors likewise replace the pending guest invocation rather
than retaining a return-only activation. This is an implementation argument,
not an allocation-free or captured-chain-destruction guarantee.

Evaluation invariants are unchanged: evaluate the callee, earlier type stages
and arguments in order; execute a final type stage before returning its result;
retain captures and package ownership; leave callable construction inert; and
preserve repeated invocation, host reentry, exceptions and post-call effects.
Unknown host callables receive their ordinary invocation, without additional
identity-discovery property or descriptor probes. No declaration-name hook,
declaration-pool search or new import participates in tail eligibility. Adding
unrelated declarations therefore cannot change an existing resolved edge or
the closure value it invokes: the open-world argument is structural and local.

Actual native-function conversion and independent-artifact calls fall outside
this transfer-identity predicate, not outside ordinary semantic correctness.
Passing the unchanged private value through opaque host storage remains inside
the predicate. Internal representation conversion is not source-level pending
work and cannot justify a retained return-only frame. The separate
[callback-boundary probes](tail-call-boundaries/README.md) preserve the broader
unlanded assertions, including their failures, without changing this boundary.

### Observed execution and remaining acceptance

The original stress/control observations use compiler SHA-256
`d6439c6eaf96c319b55fff6582b9be5037c05401e3ea790bb95e4a453498416a`.
The extended all-backend and Rust/Swift integration observations use compiler
SHA-256 `0acfe32cccf970397a204567c676b28375db693423f5ac5014b3bca7a9d803b4`.
Go's packet/static-helper refinement refreshes its three shared golden rows
and five native-interface cases with compiler SHA-256
`7cfa7886d8dd477bc54cc14bf4aeecf8dac9cfb3a4215d6fbb15af0e20330e87`.
The final compiler, SHA-256
`5a73364a45992da7b17d3a97da172d1fb998e5d57306c4c419e5be24fa563e15`,
passes 795 backend unit tests and strict Clippy. Its measured Rust/Go generated
files are byte-identical to the respective measured artifacts. Both native
fixtures additionally pass a host-panic catch followed by reuse of the same
retained deep callable, including exact partial and subsequent effect counts.
These remain bounded workflows, not claims about arbitrary host programs.
The four dynamic/Java runners use QuickJS (`rquickjs` 0.11.0), Python 3.12.15
and Java 21.0.12.1; TypeScript checks use 6.0.3, with no stack-limit increase.

| Backend | Applicability | Conformance on observed paths | Evidence and limit |
| --- | --- | --- | --- |
| JS | applies | conforms | Generated 20,000-link value and final-type chains pass, each invoked twice; ten existing semantic controls pass; 50,000-link private runtime controls pass. |
| TypeScript | applies | conforms | The same two generated chains and ten controls pass; both stress artifacts pass strict declaration checking and JS/TS runtime byte comparison. |
| Python | applies | conforms | Both generated 20,000-link chains and ten controls pass; eight private runtime tests include exact-class dispatch and host-method collision non-interception. |
| Java | applies | conforms | Both generated 20,000-link chains and ten controls pass through the independent typed host runner. |
| Rust | applies | conforms | Retained 20,001-link value/type chains pass on a 128 KiB thread stack in local/send/sync/send_sync modes at `-C opt-level=0` and `2` (eight configurations). The final fixture includes two- and three-slot named entries, unequal arguments and two package owners. The released compiler overflows on that exact fixture. |
| Go | applies | conforms | Retained 20,001-link value/type chains pass with a 128 KiB maximum goroutine stack, both default optimization and `-gcflags='all=-N -l'`. Zero-/one-slot calls, deep two-/five-slot transfers, unequal arguments, operand order, two owners and reuse pass after the packet/static-helper refinement. Four existing public-interface controls also pass. |
| Swift | applies | conforms | Three separately compiled native-interface cases pass at `-Onone`; both extended 1,024-link chain goldens pass, including stored nominal/existential transport and callable results at I32 and String. |
| Haskell | applies | conforms | Both extended 1,024-link chain goldens pass after the separately identified rank-N sum-constructor repair. Internal native/projector paths return the final invocation directly; operand demand and earlier executing type stages precede it. Actual public conversion wrappers are a separate occurrence, not an exemption for private adapters. Host-selected monad resource behavior is not established by these results. |

Both extended chain goldens and the dedicated
[`exec_forall_sum_injection`](../test-data/goldens/00_success/exec_forall_sum_injection/)
regression execute successfully on all eight backends (24 runtime rows) and
through dynamic Prime (three rows with a freshly generated interpreter driver).
Each case's invariant suite checks canonical formatting/dependencies, strict
typed skins, and Prime roundtripping. The roundtrip grammar-verifies emitted
Prime, separately builds it with the reduced compiler, and requires identical
generated target file sets and bytes. The ordinary runtime row therefore
executes the same code that the reduced compiler emits; this is not a claim
that a second native binary was executed.

The four JS/TS/Python/Java stress pairs used the original simple value/type
subjects at depth 20,000, before the [value-chain golden](../test-data/goldens/00_success/exec_general_tail_call_chain/)
and [type-chain golden](../test-data/goldens/00_success/exec_general_tail_type_stage_chain/)
acquired their additional stored-chain and callable-result subjects. Their
main-source SHA-256 values are respectively
`1213407d1a76fed9cf69eb39cde32e13fdedd211bf5f625529f13b3098c81cb1` and
`f763197afc3e5dd7e081fbf14cf6aa9adb3a158efe39e2e87126a98e6bcd6fae`.
Those extra subjects do not inherit the stress passes. The forty control runs
cover CPS/projector calls, function/type-stage ordering, host forall/interleaved
stages, generic callbacks, opaque compound transport, statement-spine captures
and same-artifact instance exchange; their phase and typed-skin checks pass.

Rust and Go's bounded hosts are [Rust](../test-data/emissions/rust/general_tail_calls/)
and [Go](../test-data/emissions/go/general_tail_calls/) emission subjects. Swift's
native cases are [private callable views](../test-data/emissions/swift/private_callable_native_views/),
[opaque substitution](../test-data/emissions/swift/opaque_structural_substitution/)
and [application-carrier visibility](../test-data/emissions/swift/application_carrier_visibility/).
Fixed-depth execution supports these workloads; it does not prove arbitrary
depth, general monadic stack bounds, or capture reclamation behavior.

### Alternatives and evidence ownership

Self-recursion-only rewriting misses captured higher-order and final-type
edges. The explored name-based identity protocol introduced effects and granted
an uncontracted host method control-flow authority. Global registries or a
shared executable runtime add deployment and lifetime obligations outside the
artifact-local decision.
Changing public callable wrappers addresses a different compatibility question.
Treating every private adapter as a native boundary would evade the predicate.

The shared specification owns the law; the plan and its [unit tests](../kio-rs/src/backends/tail_plan/tests.rs)
own result-edge eligibility. Runtime units own dispatch isolation; goldens own
language/effect behavior; emission cases own native integration facts. The
[measured representation costs](backend-performance.md#guest-contained-tail-transfers)
remain separate from stack-depth correctness: in particular, small named-call
chains incur substantial driver overhead. Integrated coverage includes the
regular and direct-Prime goldens, sampled dynamic-Prime execution, generated
programs, POCs and castles. These checks supplement the explicit per-backend
witnesses; grouping backends with similar mechanisms supplies no evidence by
itself.

# Backend performance evidence

These subject records retain measurements, tested boundaries and limitations.
They establish only the claims stated with each comparison; they neither change
[backend contracts](../specs/backends/README.md) nor require another backend to
adopt the same implementation.

These are retained results, not claims of revalidation against each checkout.
Each record retains the available workload, controls, settings and claim limits.
Any omitted source revision, toolchain version, invocation or raw-log location
remains unrecorded; it cannot be inferred from the current checkout. The results
apply to the stated experiments and assumptions.

## Sequential statement call depth

This correctness-primary comparison starts from compiler revision
`268cb4da950cad8ab2dc114e4dc5bfda0bebd826`. JS and TS share the JS renderer;
Python embeds its own evaluator. The ordinary runtimes were unchanged:
QuickJS through pinned `rquickjs` 0.11.0 and Python 3.12.15, without increased stack
or recursion limits. QuickJS retained its default 1 MiB engine stack cap;
the JS/TS runners' existing 64 MiB native worker stacks are a separate setting.
Python's `sys.getrecursionlimit()` was 1,000. A 128-effect function invoked through eight ordinary
continuations executes 1,024 prints followed by a newline. The dedicated
`exec_statement_spine_stack` golden fails on the baseline JS and Python
backends with native stack/recursion errors and passes on the repaired JS,
TS, and Python backends with the complete, byte-exact output.

The JS renderer emits one immediately invoked call scope per contiguous
`Let`/`Seq` body spine rather than one per node. Nested lexical blocks retain
initializer lookups, shadowing, and closure capture; child expression and
function boundaries remain separate. A private emitter guard checks constant
call-scope count at 4 and 64 bindings, alongside nested-function isolation.
Python advances only through those body links in a loop, preserving a fresh
environment copy at every `Let`; its private guard rejects evaluator re-entry
through either body link. The separate `exec_statement_spine_scopes` golden
checks ordered effects, own-initializer module lookup, earlier closures across
later shadows, consecutive shadows, and a host-keyword binder.
Both new cases and three existing shadowing/mixed-sequence controls passed
on all eight backends (5/5 each), with all five also passing the dynamic-Prime
differential. The direct-Prime-eligible shadowing control passed separately.

A source-equivalent eight-function, 128-effect-per-function probe also failed
on baseline JS and Python. Java, Swift, Rust, Go, and Haskell executed that
bounded probe successfully. Rust lexical blocks and Go destination statements
avoid per-statement native calls structurally; Java's evaluator recursion and
Swift's immediately invoked closures remain distinct mechanisms, so their
bounded passes are not proofs of arbitrary-depth safety. Haskell's strict
bindings and effect sequencing likewise require their own evidence.

The unchanged `egraph_equality_saturation` castle exposed the JS/TS failure on
macOS; the exact castle passed on Linux both before and after this change.
The Linux result alone does not establish native macOS clearance. These
results establish removal of avoidable sequential call frames, not a runtime
latency win, general tail-call elimination, or flattening of arbitrary bind
continuations. An ordinary bind may invoke, store, skip, or replay its
continuation; its semantics are unchanged.

## Haskell value demand

This correctness-primary repair starts from compiler revision
`6c7af9abaaed75966fe04abebfa8685d938a2574`. GHC 9.10.3 executes the same
independent hosts against baseline and repaired generated modules at `-O0`
and `-O2`. The baseline module was regenerated with a compiler built from
that revision and matched the measured artifact byte-for-byte. Its SHA-256
is `1a844d527d418476dc082393ec34d3c962e201b75606ace7af0e25c446890afd`;
the repaired module's is
`c84169a0f775fc5d86149e6f7593665fe0095c641bff39731cdd094a1133aa89`.

The retention subject selects an atom from a sum containing an atom and an
opaque payload, then duplicates that atom into a returned pair. The host
pre-evaluates an 8 MiB array, releases its input roots, and retains only that
pair. After demanding the outer pair and collecting garbage, the baseline
`-O0` left branch retains 8,417,192 bytes until a field is demanded. The
repair retains 28,448 bytes for either branch at all three checkpoints: outer
pair, first field, and both fields. The array was still allocated; this is
live-residency evidence, not a claim about peak memory. `-O2` avoids the
size-proportional retention in both versions; the repaired observation is
28,344 bytes. A selector-only control did not exhibit the original retention.

The emitter demands immediate Kio results at existing call, binding,
structural-production and facade-conversion boundaries. It does not recursively
force opaque host contents or execute stored callbacks. Pure computations
inside monadic code are demanded within their executing stage, preserving
retained type abstractions. Exact IO traces at both profiles cover type-stage,
argument and callback order, including a returned action that remains suspended
and reusable. This is not an operational proof for every possible host monad.

The `haskell/pure_value_demand` emission checks binding-relative operand,
producer-field and returned-facade demand. It fails on the baseline; removing
each of those three edges separately from generated text also makes the
checker fail despite other demand operations remaining. These mutations test
the oracle, not a runtime failure. The `exec_value_demand_preservation` golden
is separate natural-source coverage of effects, retained callbacks and type
stages, not a manufactured timing or garbage-collection assertion.

Cost probes execute 100,000 varying-input calls per row, demand both returned
fields, and check checksum 10,000,100,000. The input array is constructed
outside the measured loop. Allocation and CPU time are observed separately,
in baseline/repair/repair/baseline order. Rounded variable bytes per call:

| Profile | Arm | Direct | Nested | Choice | Callback |
| --- | --- | ---: | ---: | ---: | ---: |
| `-O0` | Baseline | 264 | 288 | 328 | 968 |
| `-O0` | Repair | 568 | 648 | 672 | 1632 |
| `-O2` | Baseline | 40 | 40 | 40 | 728 |
| `-O2` | Repair | 40 | 40 | 40 | 688 |

The ordinary runner uses `-O0`: its extra allocation and selected runtime
cost are real, not an unsupported-profile exception. For example, the two
choice CPU observations span 71–96 ns/call before and 164–177 after; direct
calls span 81–118 before and 319–323 after. Optimized choice observations span
3.9–9.0 before and 4.5–14.0 after. Two samples per arm are diagnostic evidence,
not statistical equivalence, a speedup claim, or a whole-program prediction.

Exposing generated public callable wrappers to GHC through `INLINABLE`
removes the much larger optimized overhead of the unannotated repair:
nested/choice allocation falls from 376/408 to 40/40 bytes per call, and their
roughly 99–110 ns/call observations return to the smaller ranges above.
Inlining only the two private demand helpers did not achieve this. Private
functions and helpers remain unannotated; a unit test covers the public
function, constructor and projector selection. On this small module, the
wrapper annotation increases the optimized interface from 42,763 to 48,397
bytes, object from 75,312 to 77,368 bytes, and executable by 920 bytes.
Single compile observations of 1.08 and 1.11 seconds do not establish a
compile-time regression or general scaling bound. The annotation changes no
public type, value stage or calling convention. This repair makes no general
tail-call or constant-stack claim.

Generic host calls check their conversions and result lift under a local
`forall` signature for the resolved declaration schema, then apply the actual
type arguments. This keeps strict bindings from hiding impredicative types
from GHC inference, without inspecting opaque substituted types. The
`exec_generic_callback_schema` regression retains and invokes a recursive
callback twice; its native compilation fails without the schema helper.
Guest operands remain evaluated once, in order, outside that helper.

A separate GHC 9.10.3 allocation comparison isolates a generic passthrough:
100,000 varying `Just Int` values cross `G(T)` through an ordinary host identity,
and the demanded checksum is 5,000,050,000. Before/after/after/before samples
all allocate 93,601,592 bytes at `-O0` and 11,201,112 bytes at `-O2`, including
loop and measurement overhead. No CPU timing was collected. The small
three-export probe grows from 16,762 to 17,280 generated-source bytes;
object/executable sizes remain 42,520/1,448,048 bytes at `-O0` and
46,768/1,410,968 at `-O2`. This bounds one generic-call path, not every possible
schema or a general zero-overhead claim. The original retention/cost artifact
remains byte-identical after this inference repair, so its measurements above
remain separate evidence.

## Callable-purity dispatch recovery

The measured dispatch witness executes the same host-output assertion before
and after the evaluated change. Instrumenting its generated JS function entries counted
ordinary pure factory invocations falling from 9 to 5; `relay` and the unknown
callback factory each remained at 1. This is avoided runtime work, not an
end-to-end runtime-speed claim. A bounded debug-build ABBA comparison of
`test-data/poc/elab` with `build js --no-cache` measured summed Enriched
preparation at 18.16/22.84/19.97/21.41 ms (baseline/changed/changed/baseline),
whole-command elapsed time at 37.18/36.05/45.69/53.91 s, and maximum RSS at
344444/314952/346128/321100 KiB. The runs shared compiler resources with other
work: they bound observed preparation and memory cost, but do not establish a
whole-compiler or CI speedup. Each queried expression is visited once by the
proof; declaration facts are indexed per consumer module, without traversing
callee bodies or solving a package-wide effect graph.

## JS and TypeScript host-root reuse

The JS emitter (`kio-rs/src/backends/js/emit.rs`) reuses one native-function root per
referenced exact host site inside its emitted module, without hoisting host
lookup, type/value advancement or invocation. The shared JS runtime also
serves TS. `referenced_host_roots_are_shared_without_advancing_stages` and
`host_root_identity_keeps_exact_declaring_sites_distinct` pin real emission,
delayed nullary calls, staging, unused-site exclusion, exact owner identities,
deterministic output and warm/cold emit-cache equality; restoring inline
root construction makes the firing guard fail.
Recorded roots-only discharge: the scaled `exec_rec_continuation_resume`
input used 2,000 continuation and 20,000 tail iterations, exact unchanged
stdout, one fixed JS runner and balanced execution order. Two observations
per arm averaged 13.545 seconds for control and 12.150 with root reuse
(10.3% lower). This establishes no whole-loader or broad-gate win and does not isolate
root reuse's benefit under a different callable representation.
After removal of runtime value-admission validators, a matched lexer control
takes 2.864 seconds with shared roots versus 3.124 with their construction
restored inline (8.3% lower). Both produce the same complete token output.
This single artifact-only comparison rechecks the remaining mechanism;
it is not confidence-bounded or a whole-gate measurement.

## JS and TypeScript signature-group binding

In `kio-rs/src/backends/js/emit.rs`, `wrap_js_signature_stages` uses the
already-resolved source group to bind
multiple parameters directly from the canonical product. The existing
private domain parameter is a flat cursor, not mutated shared data. The cursor reads
each spine edge once and gives the final parameter the whole tail rather than
recursively flattening it. The tested implementation preserves zero/one
parameter groups, executable type stages, source/native grouping, delayed
calls and hygienic names without deeply nested binding patterns. `static_domain_binding_is_flat_and_linear_in_arity`
covers 0/1/2/3/8/32/128/2048 parameters;
`static_domain_binding_keeps_whole_tail_and_stage_local_names` covers the
final slot, type/Unit interleaving and reserved/wildcard names. Both fail
when the old generic unpacking route is restored. An actual 128-parameter
emitted module parses and executes with the final product retained whole.
A matched source-built lexer control falls from 2.842 to 1.102 seconds
with identical complete 7,454-token output; the historical control takes
0.985 seconds. Emitted source grows from 1,626,271 to 1,769,466 bytes
(8.8%); uncached compilation takes 65.81/66.22 seconds and emission
0.964/0.932 seconds. These single observations justify the runtime trade-off,
not a compiler-time improvement, confidence bound or broad-gate prediction.
These observations concern this target realization, not other backends.

## Python and Java canonical-product access

The Python and Java runtimes (`kio-rs/src/backends/{python,java}/runtime.*`)
bind each existing signature stage directly from its canonical domain,
retaining the final product-valued tail, unary Unit, nullary groups,
lexical environments and executable type stages. Each emitter's
`guest_binding_does_not_collect_argument_slots` guards the private guest
route and retains the host collector control; both fail with the original
runtime restored. These source guards are not execution evidence.
Bounded whole-runtime probes exercise interleaved stages, captures,
shadowing, delayed guest/host effects and whole tails; the Python probe
also executes the existential-projector consumer. A previously emitted
four-export `value_plan_size` package with a private guest identity callback
performs one binary and four unary guest calls per pipeline invocation.
Python 3.12.13 ABBA samples of 20,000 pipeline calls measure
12.35/13.13 µs before and 10.86/10.81 µs after, with five slot-list
collections reduced to zero; source loading costs 5–8 ms. Python 3.12.15
repeats the same collection reduction, but its 12.31/12.78 µs before and
12.55/11.55 µs after overlap; source loading costs 6–13 ms.
Java 21.0.11 separately warmed JVM forks measure 240 fewer allocated bytes per
pipeline call; running staged controls before warmup instead measures
144 fewer bytes. Java 21.0.12.1 controls-first ABBA forks measure 120–144
fewer bytes. These allocation observations depend on JIT escape
analysis and warmup, and establish no Java latency improvement. Source
compilation costs 1.20/1.11 seconds on Java 21.0.11 and 1.31/1.21 seconds
on Java 21.0.12.1; measured JVM forks run for 1.0–1.1 seconds.
The measurements do not establish a whole-application or broad-gate speedup.

Python's single-slot product projection likewise walks only the selected
prefix, retaining the whole final tail. A Python 3.12.15 whole-runtime
evaluator probe over manually assembled projection IR in ABBA order measures
arity-16 first-slot calls at 1.365–1.462 µs before and 0.716–0.796 µs after;
final-slot calls measure 1.433–1.609 and 0.951–0.993 µs. Untimed controls
reduce 272 slot collections to zero and arity-16 field reads from 30 to
1/9/15 for first/middle/final slots, preserving 181 target evaluations,
bounds errors, unary Unit, whole product tails and nominal field access.
Source loading costs 6.8/4.6 ms; the complete bounded probe takes 3.51 seconds.
`product_projection_does_not_collect_argument_slots` fails when the original
collector call is restored. The private probe is not an emitted-application measurement.

## Typed runtime-plan storage

(`kio-rs/src/backends/{value_plan,serialized_runtime_ir}.rs`, Java emitter) —
The measured realization constructs body plans for typed-stage consumers and
interns them by shared storage identity, omitting only diagnostic binder spans.
Its key retains node/capability distinctions, exact source-use IDs, binder
names/kinds, ambient binders, roots and nominal identities. Erased and typed
execution share a serializer; erased consumers construct no admission-only
plans. Native type rendering separately consumes the common value planner. Key/projection/scaling units and their restored duplication mutations
pin firing. The JS/Python/Java `value_plan_size` emissions assert separate
absolute-byte and 4-to-32 growth bounds over all generated support, regardless
of whether the target needs runtime plans. Recorded Java totals without/with
interning are 83,659/66,215 and 337,349/157,130 bytes; its larger fixture
stores 4 body plans rather than 35. The byte counts establish neither savings from absent plans in other target
representations nor a runtime win.

## Prepared boundary-facade indexes and Python annotation batching

(`kio-rs/src/backends/boundary_facade.rs`, `kio-rs/src/backends/python/stub.rs`) — builds the live nominal/callable indexes once per facade transaction instead of rescanning package items per boundary occurrence. Protocol localization parses and walks each emitted annotation once for all captured roots, then rewrites every exact root token simultaneously; TypedDict parameter selection indexes the current candidates once and scans the current field inventory once per instantiation, while later instantiations recompute their concrete application arguments rather than reusing an argument list cached with the definition. These realizations are perf-only. Discharge (deterministic operation counts, not wall time): `live_collection_indexes_declarations_once_instead_of_rescanning_per_site` measures width 8/32 export work at 324/4,752 legacy item visits versus 48/192 indexed visits, and both-public-newtype work at 344/5,216 versus 24/96. `protocol_localization_batches_mixed_exact_roots_and_preserves_ordinal_holes` preserves mixed variance, overlapping exact identifiers, and the original ordinal hole while recording 3 annotation parses, 8 AST-node visits, and 7 rendered-identifier visits; restoring the former per-root parse/walk and replacement loops records 9/24/14 and makes the exact test fail. `typed_dict_selection_visits_current_inventory_once_per_instantiation` preserves two distinct same-shape argument inventories across one cached definition and records 32 candidate visits plus 544 identifier visits for two width-16/depth-16 instantiations; restoring the former per-candidate early-exit scan records 4,624 identifier visits and makes the exact test fail. Its empty-candidate control records zero work; removing the no-op guard records two identifier visits and makes the test fail. A temporary natural width-8/32 Python pair produced 18,250/178,720-byte stubs with maximum lines of 568/1,912 bytes and square-bracket nesting 1. The former and batched implementations emitted byte-identical `.py` and `.pyi` artifacts at both widths. Four times the authored width produced 9.79 times the `.pyi` bytes because the package-root generic inventory repeats across generated declarations; that output-shape growth is unchanged by this batching and is not presented as linear. Strict Pyright 1.1.411 passed four counterbalanced samples per width: width 8 used 0.72/0.45/0.41/0.47 seconds and 122,724/122,788/122,048/122,260 KiB peak RSS; width 32 used 0.51/0.49/0.48/0.49 seconds and 127,748/127,100/127,032/127,220 KiB. Those timings are natural-scale validation only; the deterministic operation counts are the causal measurement and give no broad-gate runtime credit.

## Backend-local prepared owner, reachability, and pending-work indexes

(`kio-rs/src/backends/rust/emit.rs`, `kio-rs/src/backends/java/{skin,emit}.rs`, `kio-rs/src/backends/swift/{skin,emit}.rs`, `kio-rs/src/backends/ts/emit.rs`) — Rust threads the exact indexed newtype-payload owner into nested forall naming rather than rescanning nominal declarations by plan pointer, Java reverses exact prepared newtype-payload plan identity to its selected nominal owner, Swift collects the exact reached-nominal set once before selecting retained host bindings, and TypeScript consumes recursive structural declarations from a pending ordered set rather than rescanning the complete discovered set after every emission. These are perf-only. Discharge (deterministic semantic-operation counts, not wall time or a claim that the underlying ordered collections are O(1)): `nested_retained_payload_foralls_resolve_their_owner_in_bounded_steps` proves identical emitted owner identity with 0/32 unrelated declarations and measures 1/1 direct semantic lookups versus 1/33 former candidate visits; `payload_owner_reverse_index_bounds_structural_operations` proves selected owner and plan-pointer parity at widths 8/32 and measures 36/528 former candidate visits versus 16/64 index-construction-plus-lookup events; `reached_nominal_index_bounds_structural_operations` proves exact-QTN selected-binding parity and measures 100/1,552 former candidate visits versus 24/96 index-construction-plus-lookup events; `recursive_structural_worklist_eliminates_ordered_rescans` proves identical ordered selection for 128 declarations and measures 8,384 former candidate extractions versus 128 pending-set extractions. Restoring each former scan at its production selection seam makes its focused guard fail while leaving the parity oracle intact, so the four indexes have causal firing coverage rather than comparator-only measurements.

# Backend performance evidence

These subject records retain measurements, tested boundaries and limitations.
They establish only the claims stated with each comparison; they neither change
[backend contracts](../specs/backends/README.md) nor require another backend to
adopt the same implementation.

These are inherited recorded results, not new measurements or revalidation.
Each record retains the available workload, controls, settings and claim limits.
Any omitted source revision, toolchain version, invocation or raw-log location
remains unrecorded; it cannot be inferred from the current checkout. The results
apply to the stated experiments and assumptions.

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

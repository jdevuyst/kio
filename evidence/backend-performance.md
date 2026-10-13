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

## Guest-contained tail transfers

These comparisons measure the implementation of the
[tail-call boundary](../specs/backends/README.md#tail-calls-within-generated-code),
starting from released compiler `3774beac81faa78c85eee7da45afb35b07742a24`.
Bounded invocation stack is the primary benefit; it is not a promise that
shallow calls or every host program become faster. Callable construction,
retained captures and their eventual destruction are separate costs.

### Rust named-entry arguments

Rust 1.99.0, edition 2024, compiled the generated library and independent host
at `-C opt-level=2`; generated code forbids unsafe code. The measurement host
uses the same forwarding allocator in both arms. Allocation counting is
disabled during timing, although its flag read remains. Captured predecessor
chains at depths 0, 1, 8 and 64 return the checked value 17. Indirect-only,
named-plus-indirect, and alternating links distinguish entry paths. Chain
construction and destruction are outside the timer. Ten samples per row use
20,000 invocations each, in released/candidate/candidate/released process order
without overlapping builds or tests.

The first step driver packed named arguments into a canonical product and then
unpacked them into a temporary vector. A private `NamedArguments` packet stores
zero, one or two values inline, with an owned vector for wider entries. It
preserves the separate native named-entry grouping and first-class single-domain
contract. Removing those two transient allocations per two-argument named hop
reduces the isolated depth-64 named-path median from 5,744.5 to 2,597 ns. Its
step grows from 48 to 64 bytes; the indirect-only control consequently rises
from 1,379.5 to 1,460 ns in that comparison.

The final production compiler has SHA-256
`0acfe32cccf970397a204567c676b28375db693423f5ac5014b3bca7a9d803b4`.
It reproduces the allocation result and passes the same source/host assertions.
Final production versus released measurements, in nanoseconds per invocation
(median and full sample range):

| Subject | Released | Production |
| --- | ---: | ---: |
| Leaf export | 25.5 (9–60) | 46 (37–56) |
| Indirect depth 64 | 1,075.5 (1,043–1,110) | 1,565.5 (1,494–1,617) |
| Named + indirect depth 64 | 1,124.5 (1,102–1,393) | 2,600.5 (2,515–2,761) |
| Alternating depth 64 | 1,165 (1,047–1,824) | 2,028 (1,985–2,170) |

All three depth-64 paths allocate 66 times per invocation in both released and
final production code. The leaf export allocates twice instead of once because
it retains the shared package owner. Named paths still perform two driver
transfers per link, owner reference-counting and an owner downcast; indirect
paths perform one transfer. The observed depth-64 overheads are about 2.31×,
1.46× and 1.74× for named, indirect and alternating paths respectively. These
are costs of this implementation, not proven lower bounds for stack safety.
Small-call rows vary substantially; they establish no universal latency ratio.

The separate [native fixture](../test-data/emissions/rust/general_tail_calls/)
executes 20,001-link captured value and type-stage chains twice across two
owners on a 128 KiB host thread, in all four thread-safety modes at optimization
levels 0 and 2. Two- and three-slot named entries, including unequal arguments,
are exercised. The released compiler's exact strengthened fixture overflows
the native stack; final production passes. Wider-entry vector allocation is
not measured by the two-argument timing subject.

### Go reusable named entries

Go 1.27.2 on linux/amd64 built the generated package and fixed independent
host with ordinary `go build` settings. The source exercises indirect-only,
named-plus-indirect and alternating captured predecessor chains at depths
0, 1, 8 and 64, checking result 17. Allocation measurements use
`testing.AllocsPerRun(100, ...)`; timing uses ten samples of 20,000 calls per
row in released/candidate/candidate/released process order, without overlapping
builds or tests. Both exclude chain construction. All samples are retained.

The original driver allocated a product domain and unpacking slice per named
transfer. A concrete first/second/rest packet removes that packing. A remaining
invoker literal inside the generic package method still captured Go's generic
dictionary, even after its body used only an exact private method interface.
Hoisting that invoker to one nongeneric package-level helper per body removes
the closure allocation. It asserts the same private method interface on the
retained owner; it adds no registry, public callable type or runtime type
argument. Wider entries retain one overflow slice.

Unmodified production output from compiler SHA-256
`7cfa7886d8dd477bc54cc14bf4aeecf8dac9cfb3a4215d6fbb15af0e20330e87`
reproduces the measured allocation reduction:

| Allocations per invocation | Released | Original driver | Final driver |
| --- | ---: | ---: | ---: |
| Leaf export | 0 | 0 | 0 |
| Indirect depth 64 | 1 | 3 | 1 |
| Named + indirect depth 64 | 1 | 195 | 1 |
| Alternating depth 64 | 1 | 99 | 1 |

Every measured final chain depth has one allocation per public invocation, independent
of depth. The 56-byte argument packet enlarges the common step from 80 to 136
bytes, including on indirect-only paths. Final timing remains materially slower
than release (nanoseconds per invocation, median and full sample range):

| Subject | Released | Final driver |
| --- | ---: | ---: |
| Leaf export | 14 (12–16) | 13 (8–19) |
| Indirect depth 64 | 512 (500–556) | 1,848 (1,815–1,918) |
| Named + indirect depth 64 | 727.5 (659–1,287) | 3,911 (3,880–4,476) |
| Alternating depth 64 | 865.5 (663–1,346) | 2,909 (2,875–3,718) |

Those depth-64 medians are 3.61×, 5.38× and 3.36× the released times. Named links
perform two driver transfers plus the helper's interface dispatch; indirect
links perform one transfer. The larger result copy and dispatch remain, but
these measurements do not attribute their costs individually or prove that
the overhead is unavoidable. Shallow rows and released alternating timings
are noisy. The benefit established here is bounded invocation stack, not a
general speedup or acceptable overhead for every workload.

For this subject, generated body/runtime source grows from 9,807/4,255 bytes
in release to 13,336/5,664 bytes; the executable grows from 2,648,354 to
2,658,786 bytes. Ten private static helpers contribute linear source text;
unused helpers can be removed by the native linker. Public host, FFI and shape
files are byte-identical before and after the packet/helper repair.
The [native fixture](../test-data/emissions/go/general_tail_calls/) separately
checks zero-, one-, two- and five-slot calls, operand order, two owners, repeated
invocation and final type stages. Its 20,001-link chains pass with a 128 KiB
maximum goroutine stack at default settings and with `-gcflags='all=-N -l'`.
Four existing public-interface controls also pass. The two-slot cost experiment
does not measure wider-entry allocation or arbitrary host workloads.

### Swift private entries

Swift 6.4 on x86_64 Linux compiled the same generated package and independent
host at `-Onone` and `-O`. A host loop builds a captured predecessor chain;
the stored variant transports that callback and a mark through a product and
generic private nominal, then invokes a named helper at each link. Construction
has no effects; each invocation checks one leaf event, exactly one tick per
link and result 42. Six alternating samples per arm execute 3,000 invocations
at depths 0, 1 and 64. Timers include invocation-time helper construction and
dispatch, but exclude chain construction, native-view creation and destruction.
Other builds and tests were held during the comparison.

The initial private-entry implementation allocated a transfer cursor even for
a body already classified as terminal by the shared tail plan. Keeping that
terminal/resumable distinction removes the cursor and ignored-cursor wrapper
for terminal entry calls. Terminal does not mean pure: its effects and non-tail
calls still execute. All targets remain the same private entry type, so an
active driver can finish at a terminal entry without starting another driver.

Median nanoseconds per invocation (full six-sample range). These are two
separate paired experiments, not a single three-arm run:

| Comparison | Profile / subject | Before | After |
| --- | --- | ---: | ---: |
| Released → initial private entry | `-O`, leaf | 85.7 (81.4–94.6) | 233.6 (225.9–263.7) |
| Released → initial private entry | `-Onone`, leaf | 144.4 (140.2–157.2) | 339.8 (325.9–352.8) |
| Released → initial private entry | `-O`, simple depth 64 | 26,837.6 (24,637.2–29,682.5) | 29,970.5 (29,158.8–30,642.3) |
| Released → initial private entry | `-O`, stored depth 64 | 146,919.9 (143,814.1–153,621.2) | 140,840.4 (139,247.8–147,120.1) |
| Initial → terminal fast path | `-O`, leaf | 230.1 (217.4–237.0) | 165.4 (160.9–175.2) |
| Initial → terminal fast path | `-Onone`, leaf | 328.6 (325.7–347.5) | 280.2 (266.8–318.6) |
| Initial → terminal fast path | `-O`, simple depth 64 | 30,071.0 (29,750.3–33,998.7) | 30,455.0 (29,568.7–32,648.9) |
| Initial → terminal fast path | `-O`, stored depth 64 | 141,152.9 (137,776.4–145,156.9) | 140,767.2 (135,540.2–151,215.5) |
| Initial → terminal fast path | `-Onone`, simple depth 64 | 45,807.6 (43,077.7–49,658.2) | 50,853.2 (45,991.8–56,196.9) |

The leaf rows are the later repeated leaf measurements. The first depth-0/1
rows had substantial startup variation and no explicit warm-up, so they do not
establish steady-state latency. The fast path improves the later leaf by about
28% at `-O` and 15% at `-Onone`; it still costs more than the released leaf.
The `-Onone` simple-chain median increases about 11%, with overlapping ranges.
Neither comparison establishes a general speedup or an allocation census.

The terminal discriminator adds a tag and dispatch branch. This small artifact's
executable grows from 204,352 to 210,664 bytes at `-Onone` and 144,368 to 146,232
at `-O`; native compilation was not timed. Final compiler SHA-256
`0acfe32cccf970397a204567c676b28375db693423f5ac5014b3bca7a9d803b4`
regenerated and passed the same clock-disabled controls at both profiles. Its
timed invocation path matches the measured terminal artifact; the only code
difference is a terminal forall constructor used during untimed construction.
Host and FFI files remain byte-identical to release. Private unit assertions
pin the terminal-before-cursor path and its constructor selection; native
interface and executing-type-stage tests supply separate semantic evidence.

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

# Call-stack experiment: deeper execution at a measurable cost

This report compares four cumulative call-stack changes with released Kio
0.2.3. The scalar, depth, compiler and bounded application results below have
been independently checked. This report records the evidence and the decision
to retain the implementation as an experiment.

## Outcome

The experiment substantially improves the tested depth limits on Python,
QuickJS and Java. Its complete implementation passes depth 10,000 in every
selected backend/mode combination. That benefit is not free: shallow calls
with pending work are often slower, some callback paths pay large overheads,
and one Rust application's generated source becomes 4.11 times larger. The
same application's repeated public entry takes about 2.24× baseline invocation
time in the matched comparisons.

The intermediate stages are not uniformly better. Python's pending-call depth
gets worse when cooperative public callbacks are introduced, before the final
stage fixes that path. Swift's tail-only stage crashes in two deep callback
tests that the released baseline completes. The final implementation completes
those tests, but success at a finite depth is not a proof of unbounded execution
or constant memory.

**None of the four stages is adopted into main.** The complete implementation
is retained on the experimental branch. The depth benefit is real in the tested
cases, but the API migration and shallow-call costs are material. The completed
application supplement shows a material cost in one larger program too; it
cannot establish a universal acceptable slowdown. This decision does not rule
out reconsideration for different workloads or a different implementation.

## What was compared

Each row is cumulative. All five compilers were built from the exact frozen
sources using the same release-build procedure; the baseline source is the
immutable published 0.2.3 release, not a reconstructed approximation.

The cumulative implementation is retained on `experiment/call-stack-safety`.
The exact revisions below identify the measured code; the moving branch tip
does not replace those identities.

| Key | Revision | Change |
| --- | --- | --- |
| B | `eefe9f09133f9eac5ea044603ee10362b5da5f25` | Released 0.2.3 baseline |
| T | `3f2e24d6bcb6f6b1ddd7bced0951af13679f5540` | Guest-tail execution |
| C | `b4f85de2f47ad270bc9f69bfc834f7a1f73a0645` | Cooperative public callback transport |
| S | `b2854823d438e4c2ad1fe23aa8ee73bde56439b1` | Shared executable-stage planning |
| P | `80f185963adf3aaffe5b14e1748c2d6431d5a97c` | Managed calls with pending work |

The stage commits were signed for publication after measurement. The signed
revisions below have exactly the same source trees as their measured counterparts
above; signing changed commit metadata, not the tested compiler. The release
baseline is unchanged.

| Key | Signed revision | Source tree |
| --- | --- | --- |
| T | `b65a1ac6bc8866d15ee984e316122a12f29057f7` | `54cc57d7989af03e6e34a18c1b206a658b64e311` |
| C | `85e3eb361ef7ec4dafe8cf4e2428eddcf71162c1` | `31db707cab69c27e4cd8967b53b524187852ce69` |
| S | `57a3b82475da84a90b9efaec37676f32f4c1f699` | `dc2f8cdba73e07e9cb63488d409a60a6677bf426` |
| P | `94bf33845c1fa701521a08e6d0efe6a496607e0e` | `0d90a637f8d09e18a0b9f29580117c370f45cf94` |

The logical scalar workload constructs and invokes direct calls, stored tail
or pending chains, and returned callbacks with an independent checksum/effect
oracle. Python uses 3.12.15 with default recursion policy. Isolated JavaScript
uses a release-opt-level-3 launcher with rquickjs 0.11.0. Rust guest/driver
artifacts use opt-level 2; Swift uses `-O` and separate package modules;
Haskell uses `-O2` and the default RTS policy. Go uses its ordinary compiler
defaults. Java's fresh and fixed-warmup rows remain distinct. The same pinned
tool configuration serves all arms within each comparison; no stack-limit
increase or profiling mode is used to turn failures into passes.

Tail calls finish by handing control to another guest call. Pending calls must
also remember work to do after that call returns. Public callback transport
preserves the generated callable when a host stores or passes it; wrapping it
in a new native closure creates a different boundary.

### The value and cost of each stage

| Stage | What it contributes | What the evidence does not let us overlook |
| --- | --- | --- |
| Guest-tail execution | Python and QuickJS selected tail chains reach 10,000; no ordinary host-API migration. | Swift callback-tail and callback-pending crash at 10,000 after passing 4,096. QuickJS's shallow guest-to-native callback probe is already about 2.07× baseline time. |
| Public callbacks | Cooperative callables survive unchanged host storage and compatible package boundaries; Swift has separately compiled package witnesses. | Most host migration occurs here. Go's depth-8 returned tail callback is about 1.99× the preceding stage's time. Swift improves relative to its tail-only stage, but remains slower than baseline. Python pending depth is temporarily worse. |
| Shared planning | Centralizes stage/capture decisions and their ownership across applicable backends. | No standalone speed win or additional tested depth win is established. It is an architectural change, not a demonstrated performance optimization. |
| Managed pending work | Python, QuickJS and Java pending cases progress from recorded failures to depth 10,000. | This is the largest shallow pending-call cost increase on QuickJS and Rust; generated Rust continuation machinery also expands substantially. Other selected baselines already survive 10,000. |

For the repeated Rust application entry, matched time relative to baseline is
1.120× after tail execution, 1.084× after public callbacks, 1.116× after shared
planning, and 2.236× after managed pending work. These observations are not
monotone stage improvements; the last stage has the largest measured cost here.

## How deep did the tests get?

Entries show the highest **successful tested depth**. Parentheses give the next
tested failure, not an exact threshold. A 10,000 entry means that the bounded
ladder stopped there. Failed cases were retained, not retried with raised limits.

| Selected subject | Baseline | Tail | Callbacks | Shared planning | Pending work |
| --- | ---: | ---: | ---: | ---: | ---: |
| Python tail / callback-tail | 64 (fails 256) | 10,000 | 10,000 | 10,000 | 10,000 |
| Python pending / callback-pending | 64 (fails 256) | 256 (fails 1,024) | 64 (fails 256) | 64 (fails 256) | 10,000 |
| QuickJS tail / callback-tail | 256 (fails 1,024) | 10,000 | 10,000 | 10,000 | 10,000 |
| QuickJS pending / callback-pending | 256 (fails 1,024) | 256 (fails 1,024) | 256 (fails 1,024) | 256 (fails 1,024) | 10,000 |
| Java pending | 256 (fails 1,024) | 256 (fails 1,024) | 256 (fails 1,024) | 256 (fails 1,024) | 10,000 |
| Swift callback-tail / callback-pending | 10,000 | 4,096 (crashes 10,000) | 10,000 | 10,000 | 10,000 |
| Rust four modes; Go pending; Haskell callback-pending | 10,000 | 10,000 | 10,000 | 10,000 | 10,000 |

Python reports `RecursionError`, QuickJS a maximum-call-stack error, and Java
`StackOverflowError`. Swift's two failures are SIGSEGVs. Their stack-local
faulting writes support stack pressure as the leading explanation, but the
retained evidence does not conclusively prove the cause or precise failing
phase. The tail-only stage does not promise cooperative transport through
arbitrary native callbacks; nevertheless, these are real regressions in the
tested public callback usage and remain part of the tradeoff.

Go and Haskell retain their legitimate growing-stack mechanisms. TypeScript's
scalar runtime evidence reuses verified identical emitted JavaScript, alongside
five strict declaration checks; it is not a second set of JavaScript runs.

## What do ordinary shallow calls cost?

These are median invocation times in **microseconds per repeated call**, not
whole-process startup or application throughput. Depth 8 contains a seed plus
eight wrappers. Ratios are medians of within-block comparisons: they need not
equal the ratio of the two displayed medians. Greater than 1 means slower.
Primary/combined rows have ten matched blocks; targeted Go/Java/Swift/Haskell
rows have five. The complete stage-by-stage values and observed ranges remain
in the identified runtime analysis; this table highlights one pending path per backend and
the particularly sensitive QuickJS callback paths.

| Workload | Baseline µs | Final µs | Final / baseline | Final / shared-planning stage |
| --- | ---: | ---: | ---: | ---: |
| Python pending, depth 8 | 75.154 | 83.465 | 1.152× | 1.024× |
| QuickJS pending, depth 8 | 7.070 | 22.672 | 3.202× | 2.507× |
| QuickJS returned callback-pending, depth 8 | 2.768 | 16.538 | 5.951× | 5.029× |
| QuickJS guest call to native callback, depth 0 | 0.442 | 2.287 | 5.172× | 1.691× |
| Rust pending, depth 8 | 0.262 | 0.630 | 2.397× | 2.346× |
| Go pending, depth 8 | 0.329 | 0.755 | 2.145× | 0.967× |
| Java pending, depth 8, fresh | 24.610 | 35.261 | 1.370× | 1.344× |
| Java pending, depth 8, fixed warmup | 11.001 | 16.505 | 1.457× | 1.583× |
| Swift returned callback-pending, depth 8 | 3.760 | 6.842 | 1.825× | 1.367× |
| Haskell returned callback-pending, depth 8 | 1.814 | 1.743 | 0.974× | 1.025× |

For context, the simpler depth-0 direct-call probes have final/baseline ratios
of 1.140× on Python, 1.058× on QuickJS and 1.155× on Rust. The large pending
ratios are therefore not a universal multiplier for every call. QuickJS's
“native” row still enters a guest function which calls a native callback; it is
not a host-only control. Its absolute median difference is about 1.85 µs.

The final runtime run completed 1,225 samples without failures. Combining the
six preserved pilot cells with their continuation produces 1,375 unique
observations; overlapping summary views are not independent replications.
Observed ranges are not confidence intervals. Java's fixed warmup is exactly
10,000 calls, not a demonstrated steady JIT state; Rust's shortest controls
remain short at their fixed repetition cap. QuickJS results do not establish
Node/V8 performance. Haskell's separate host IO-bind controls are not imported
Kio `do`/`bind` or pending-stack coverage.

Package setup, construction, warmup and root release are recorded separately. The driver's
“total phases” field is their sum with invocation, excluding intervening work;
outer process wall is the separate end-to-end observation. No host-loop or
startup subtraction invents a pure guest execution time.

## Larger application: order-book matcher

The unchanged application processes a fixed 19-event scenario, builds fresh
domain state, formats its results and checks all 2,509 expected output bytes.
All 275 timing samples passed across seven cells, including every baseline
case, with no failed/censored rows or retries. Order-book cells have ten matched
blocks per arm; the three Rust control cells have five. Brackets below are the
observed matched-ratio range, not confidence intervals.

| Workload / measurement | Baseline median | Final median | Final / baseline [range] | Final / shared planning |
| --- | ---: | ---: | ---: | ---: |
| Order book, QuickJS process | 50.726 ms | 85.694 ms | 1.723× [1.474, 2.198] | 1.457× |
| Order book, Python process | 117.382 ms | 129.110 ms | 1.123× [0.960, 1.287] | 1.035× |
| Order book, Rust process | 5.699 ms | 6.956 ms | 1.182× [0.886, 2.257] | 1.100× |
| Order book, repeated Rust entry | 183.876 µs | 422.120 µs | 2.236× [2.155, 2.567] | 2.061× |
| General tail-chain control, Rust process | 11.720 ms | 8.066 ms | 0.843× [0.580, 1.200] | 0.899× |
| Pending type-stage control, Rust process | 5.141 ms | 5.860 ms | 1.158× [0.898, 1.777] | 1.271× |
| Distinct-bind-type control, Rust process | 5.238 ms | 4.902 ms | 0.936× [0.756, 1.157] | 0.975× |

The repeated entry runs 1,000 times per sample with no warmup, reconstructing
the Book and arrivals on every call. Its final/baseline ratio exceeds 2.15 in
every measured block. The short Rust process row measures a different boundary
and has substantial startup/scheduling variation; its smaller ratio does not
contradict the repeated-entry cost. Likewise, the small control process rows
do not isolate the internal cost of tail calls, type stages or binds. One
fixed application is not a representative application distribution.

The JS/Python rows use normal default-dev public runners; JS includes that
QuickJS engine/host profile, and Python uses the pinned external interpreter.
These process costs include startup/loading/host/entry/output and must not be
pooled with the isolated release-launcher invocation measurements above. The
repeated Rust entry uses opt-level 2 and a memory print sink: it includes domain
reconstruction and formatting, but excludes package creation, output checking
and sink clearing from its invocation timer. It is not varied exchange
throughput or an isolated matching kernel. Its companion full-loop median is
186.957 ms at baseline versus 425.304 ms finally, with matched ratio 2.217×;
including validation and clearing does not remove the observed cost.

## Host integration changes

Most migration is introduced by public callback transport, not by the final
pending-work stage. Tail-only preserves the ordinary interface; shared planning
and pending work retain the callback-stage interface.

- **JavaScript/TypeScript and Python:** preserve generated callables unchanged
  and lift newly authored native callbacks. JS/TS add factory `lift`/`liftStage`;
  Python's existing `kio_lift`/`kio_lift_stage` become the required protocol
  adapters. Python hosts already following that recommendation need no spelling
  change. Ordinary named host-record methods remain ordinary methods.
- **Go:** native function values become `Callable`/`Func`; use `f.Call(x)` and
  extract `f.Call` for native APIs. Type-stage functions have the corresponding
  `TypeCallable`/`TypeFunc` interface. First-class Unit callbacks receive Unit.
- **Swift:** `(A) -> R` becomes `KioFn<A,R>`; `f(x)` still works, while native
  extraction uses `f.callAsFunction`. Callback-containing host types change too.
  Independent packages link one compatible emitted `KioRuntime` support module.
- **Rust/Java:** existing `KioFn1::new`/`.call` and `Shapes.Fn1`/`.apply` remain.
  Do not replace a returned carrier with a forwarding native closure when its
  cooperative transport matters.
- **Haskell:** first-class Unit callbacks consistently become `() -> m r`;
  named nullary declarations still return actions directly.

For example, Swift changes `let f: (Int32) -> Int32 = { $0 + 1 }` to
`let f = KioFn<Int32, Int32> { $0 + 1 }` with `import KioRuntime`. Python's
`f = generated_module.kio_lift(lambda x: x + 1)` works across all five revisions.
Rust's ordinary `.call(value)` interface is unchanged.

The experiment adds no ordinary asynchronous or error-result API. Host errors
and synchronous reentry keep their existing contracts; compatible public types
are still required between artifacts. Native extraction/relifting does not
restore unchanged guest transport, and arbitrary host callbacks, reentry and
capture destruction are not covered by a universal guest-stack guarantee.

## Compiler and generated-source costs

The secondary compiler panel ran 180 successful commands: three workloads,
check plus JS/Python/Rust emission, five revisions and three matched rotations.
Each command received identical fresh source copies with Kio semantic caches
disabled. Filesystem pages and native/compiler caches were not made cold.

| Compiler workload | Baseline median ms | Final median ms | Matched final / baseline |
| --- | ---: | ---: | ---: |
| Small benchmark → Python | 56.229 | 67.722 | 1.236× |
| Elaborator demo → Rust | 3,377.689 | 3,352.838 | 0.993× |
| Order book → Rust | 1,495.618 | 1,556.370 | 1.070× |

These are descriptive launcher-inclusive observations with only three rotations,
not confidence bounds or compiler-wide nonregression. The small benchmark has
substantial fixed launch overhead. Native preparation with configured sccache
is a different measurement: it is not a controlled cold rustc comparison.
The original 20 application native pairs also ran without active compiler
admission; their serial preparation records do not evaluate scheduling policy.

Order-book Rust output grows from **269,536 to 1,108,812 bytes (4.11×)**. Tail,
callback and shared-planning outputs are byte-identical at 328,586 bytes.
Almost all additional final-stage bytes are in the generated library: 466
control-block helpers and 302 resume methods, with live-value transport.
The 133 original method identities and inspected guest/host call occurrences
remain unchanged. This explains substantial per-control-edge scaffolding, not
observed copying of the guest program's subtrees; one application cannot prove
an asymptotic growth law. Generated bytes are not executable size, allocation
volume, native compilation time or runtime slowdown.

## Correctness caveat and decision limits

The final stage's broad gate passed 91/91 groups. The shared-planning stage's
original full gate remains **RED, 89/91**. Its backend matrix and complete
package/hygiene recheck passed, and an unchanged-artifact editor replay passed
8/8, but the original editor failure's cause remains unresolved. Repeating the
full backend matrix was waived solely for that editor finding; that waiver
does not turn the original gate green. Historical gate durations are not
controlled performance comparisons and do not establish ordinary-CI throughput
or scheduler-policy improvements.

Process RSS is only a coarse high-water observation. Several final native
drivers cluster near a 13.35–13.48 MB floor, while the earlier QuickJS/Rust
window was near 14.9–15.0 MB; fork/pre-exec launcher memory can contribute.
This does not rank target-only memory or establish savings. Java pending does
show a larger final-stage process high-water, but that is not a retained-heap
or leak measurement. Allocation totals, live/retained heap, collection latency,
capture destruction and native-stack byte slopes were not measured. Reverse
root clearing does not supply those missing metrics.

This bounded evidence is sufficient for a qualified adoption discussion
without another open-ended benchmark or
profiler campaign. It is not a blanket performance or lifetime certification.
The decision weighs the tested depth gains against the migration and costs
for the intended workloads. Retaining the experiment preserves the evidence
for reconsideration; neither a favorable application nor a passing final gate
makes adoption automatic.

## Source and result anchors

The host-interface claims refer to the backend specifications and matching
host guides at the exact revisions above. The shared execution boundaries are
in `specs/backends/README.md`; callback forms are in the per-backend specs and
`docs/hosts/` guides. Their experimental contracts must not be mistaken for
the released baseline's interface. The domain source is
`test-data/castles/order_book_matcher` at B. Rust controls use
`exec_general_tail_call_chain` and `exec_pending_type_stage_chain` at P, and
`exec_monadic_distinct_bind_types` at B, under `test-data/goldens/00_success`.

SHA-256 result identities distinguish these observations from other runs:

- Descriptive runtime analysis: `2c7257ebd8a243c32da580df1ccbeb83540675e0e7dfd32ac0ab1a279a860130`.
- Unique runtime raw-record digest: `53c9a80700fa321dae01ed3f5b53c86fa5d3b21db05d8fb235543902b2f59087`.
- Initial depth completion: `ff12c50c76b92916df488334bdf7046f03657bf058c413a37dc46745ee0a2b72`.
- Depth-extension completion: `ec44aa7fa04174998de2626b7ca8b9349b6c804a2e5f4a9fddb3ae5750af1d0c`.
- Compiler analysis: `3b3dddc43d207b16bcec854c9435ed84c4e633cdd9f8ddf09c78a6582af27ec7`.
- Application analysis: `4477dc54bdd38a4455573d6efaf9727b92c358768b435e58e0263266691c14fa`.
- Unique application raw-record digest: `e25230bb37880b8773bc64df55c4032728da115879c026e7f897f0ccc7259466`.

# CI performance evidence

Measurement and acceptance records for test infrastructure, resource scheduling
and CI build preparation. Kio compiler and language-tooling measurements are in
[compiler performance evidence](compiler-performance.md).

Historical records do not automatically revalidate the current implementation.
Each record preserves its available workload, controls, source/test anchors,
settings, results and claim limits. Some inherited summaries do not identify the
measurement's source commit, exact toolchain, full invocation or raw-log
location. An omitted identifier or setting remains unrecorded; it must not be
inferred from the current checkout. Claims apply to the stated experiment and
assumptions, not to an unspecified current toolchain or whole-gate comparison.

## Runner compile profiles

Source: `ci/infra/kio-test-runner-rs/src/shared/opt_profile.rs`.

The one perf-only lever they invited — threading rustc `-C codegen-units` on the `optimized` profile, where codegen dominates more than at `opt-level=1` — was **re-measured and rejected**: on the largest emitted crate (`dyn_load_prime`, cache-miss, several iterations) codegen-units 1 vs nproc is a wash (~1.00x) at both `opt-level=1` and `opt-level=2`, because rustc's front end is single-threaded and codegen stays a small fraction even at `-O2`. It is therefore not threaded on any profile.

## Native resource engine and adaptive compiler feedback

Source: `ci/infra/kio-ci-scheduler-rs/`, reached through `ci/schedule.sh`.

Keyed bootstrap reuse and inherited readiness-hook reuse retain their separate discharges: cold Linux prepare measured 2.19 seconds / 208,052 KiB versus about 0.31 seconds for warm resolve, and canonical inherited re-entry invokes zero readiness children rather than one. Those measurements give no credit to feedback.

Pressure backoff resets the adaptive target to one instead of halving it: a
high target can otherwise still admit another producer while pressure is
observed. Deterministic policy and admission tests cover that distinction,
one-producer progress, and paced healthy recovery. This is admission control,
not a bound on the memory growth of already-running producers.

A counterbalanced `A1/B1/B2/A2` comparison screened normal-path overhead using
`exec_host_loop_map`, `exec_host_loop_fold`, `exec_recursive_type_groups`,
`exec_dict_fold_list_interop`, `exec_qualified_poly_call`, and
`ffi_export_poly_roundtrip` on Rust, Go, Java, Swift and Haskell. Each arm used
the same compiler and target-language tools, default admission, a fresh runner
artifact cache, and matching scheduler and native-runner binaries for its policy.
Configured compiler caches were preserved; native runners used no compiler
wrapper. All 120 case/backend rows passed. Every arm admitted the same 42 native
compiler commands and 50 Kio commands. Wall times were 51.21/50.24/50.89/49.94
seconds: baseline mean 50.575 seconds, repaired mean 50.565 seconds, with no
meaningful difference in this workload. Before/after Linux memory-pressure
snapshots reported zero `avg10`; these snapshots and admission traces do not
establish the cost of sustained pressure or whole-broad-gate equivalence.

### Selective large-native-input admission

Source: `ci/infra/kio-ci-scheduler-rs/src/compiler_admission.rs` and the
Rust, Go, Java, Swift and Haskell test-runner compile adapters. Candidate
`498312282a659d474b65bd0adabfa18f963e73a5` is compared with its parent
`fc1b5f5c32925dbac443fccaaf398ae4054039d5`.

The extra one-root restriction applies only to complete project-source
closures of at least 6 MiB. Smaller and unmeasured inputs keep ordinary
fixed/adaptive admission. Counts include assembled drivers and the sources
behind linked local artifacts; they are computed for the invocation, not
stored in an inventory or learned across runs. Existing retained source
buffers supply most counts. Rust coexistence additionally collects and drops
each package's source buffers once. This is a source-volume precaution, not a
RAM estimate: a large but cheap input can be unnecessarily serialized, while
a small or unmeasured input can still consume substantial memory.

A focused ordered control/candidate pair ran the same nine case oracles:
`exec_dyn_load_exact_descriptors` on Haskell, `exec_hello_world` on all five
native routes, the `dyn_load_prime` POC on Rust,
`exec_dyn_load_wrapped_params` on Go, and the Rust
`forall_residual_stages` emission. With one common frozen Kio binary, matching
target-language tools, default admission and fresh private runner-artifact
caches, all passed in 159.52/159.51 seconds. Each arm completed eleven observed
runner-native commands plus two direct Rust compiles. In the candidate, the
ten small native commands and both unmeasured direct compiles ran during the
9,227,567-byte Haskell compilation. The Rust package-plus-driver and Go
negative controls measured 4,829,195 and 4,392,966 bytes respectively.
This pair establishes practical parity, not a throughput gain; shared compiler
and operating-system caches were not reset.

A candidate-only two-heavy row added Haskell `exec_dyn_load_host_callback`
and retained the five-route Hello burst. All seven oracles and nine observed
native commands passed in 239.12 seconds. The second heavy request, with
8,915,820 source bytes, waited 89.437 seconds on `native-tree-active` and
started after the first heavy compiler ended. Their separately observed
maximum RSS values were 6,307,376 and 6,218,160 KiB. This measures exclusion
of the risky overlap, not aggregate memory saved or an OOM prevented. Small
compiles overlapped the first heavy but finished before the second queued;
bypass of an already queued heavy is established by deterministic tests,
not by this runtime row.

The ordinary broad comparison used one warm checkout, separately copied and
verified identical initial native-cache contents, formatter seed `20261019`,
and the following command without work/compiler-capacity overrides:

```sh
sh ci/all.sh SAMPLE_IMPL --impl-seed=20261019 --case-seed=20261019 \
  --gen-seed=20261019 --gen-count=200
```

Both arms passed all 91 checks: control 2,089.89 seconds (34:49.89), candidate
1,775.99 seconds (29:35.99). The candidate's observed wall time was 15.0%
lower, but this is not an isolated scheduler speedup. Control ran first;
normal compiler/runner preparation remained inside both gates. The candidate
started with the control's final build products, and its all-feature test
compilation took 36.10 seconds versus 3m50s for control. Shared compiler and
OS caches and changed Git provenance also limit attribution. The result is a
bounded ordinary-gate cost screen, not a cold-build or full-matrix timing.
Selections, baseline/tool identities and completed case results matched, as
did the normalized command multisets of all 654 native compilations per arm.
Both gates used identical temporary cache-routing and formatter-seed edits;
their dirty trees therefore skipped the file-ownership subcheck. Both edits
were restored byte-for-byte, and the separate clean-tree GitHub lint passed
the actual file-ownership check.

The broad traces also expose a real false-positive cost: one measured-large
Java compilation waited 188.492 seconds while a large compilation held the
precaution slot, although the six large Java commands themselves took only
about 3–5 seconds each. Source volume does not distinguish those cheap jobs
from the observed 198.12-second Swift package compilation. The shorter candidate
runtime therefore does not prove isolated scheduler nonregression or eliminate
this individual latency cost.

The delayed Java workload was `exec_dyn_load_type_only_contract`, with
8,660,086 source bytes. Its candidate compilation took 2.96 seconds and peaked
at 306,056 KiB RSS. The control's matching command took 2.48 seconds at
351,440 KiB RSS and ran entirely within the Swift `exec_dyn_load_wrapped_params`
package compilation (211.76 seconds, 2,285,476 KiB RSS); both exited zero.
Thus the retained control already demonstrates successful overlap for this
pair. These are individual process peaks, not simultaneous aggregate memory
or a bound for other Java inputs, compiler versions or environments.

`native_input_classification_edges_unknown_and_overflow`,
`ordinary_root_passes_a_blocked_native_waiter`,
`native_release_restores_fifo_and_cancellation_exposes_next_waiter`, and
`native_class_preserves_aggregate_fixed_and_adaptive_ceilings` guard the shared
policy. Production-adapter tests cover upstream sources, drivers, coexistence,
cache hits and inherited leases; omitting either Java driver's contribution
was observed to fail its boundary test. The measured benefit is controlled
heavy-root overlap with retained light-work concurrency, not faster compilation.
Whole Cargo invocations, internal compiler fan-out and unmeasured commands
remain outside the extra restriction. Native measurements were on Linux;
they do not establish macOS/Windows resource behavior or universal OOM safety.

### Within-run native-memory prediction experiment

Collection candidate `688bd8245b571b99cd3e0bfad6a19996135d1f22`, based on
`7bd128ab1`, adds opt-in input receipts without changing admission, cache keys
or coverage. One ordinary `SAMPLE_IMPL` gate, with all three seeds set to
`20261021`, 200 generated programs, existing caches and default concurrency,
passed 91/91 checks in 2,505.40 seconds (41:45.40). This is collection and
integration evidence, not a scheduler-speedup comparison.

The collection contains 611 authenticated requests and 437 usable successful
memory observations. The other 174 observations measure Rust cache-wrapper
clients rather than compiler memory and are excluded from training and error
metrics, not from the request inventory. There are no missing joins, failed or
censored commands, or incomplete source receipts. Cache hits and native commands
outside the runner's observation boundary are not counted. Measured tools include
GHC 9.10.3, Go 1.26.9/1.27.2, Java 21.0.12.1, Rust 1.99.0 and Swift 6.0.3/6.4.

The frozen comparison trains only on successful same-cohort compilations that
completed before each request. Cohorts separate compiler identity, target,
stage, flags and resource environment. The size-only ridge model predicts log
peak memory from log input bytes. The richer ridge adds compiled-byte share,
largest-file share, `log1p` nonblank lines, and `log1p` raw `(` and `<` counts. These
counts include strings and comments; they are not parsed syntax or a complete
dependency graph. Both require eight prior observations, use ridge coefficient
1 and standardize from prior data only. Point predictions and a fixed 1.5
multiplier are reported separately; the multiplier is not a safety bound.

| Model with 1.5 multiplier | Predicted requests with usable outcomes | Underestimates | Worst actual/predicted ratio | Largest deficit |
| --- | ---: | ---: | ---: | ---: |
| Prior maximum | 426 | 7 | 22.43 | 7,199 MiB |
| Nearby-size envelope | 376 | 1 | 1.055 | 14 MiB |
| Size-only ridge | 369 | 6 | 13.09 | 5,653 MiB |
| Richer ridge | 369 | 6 | 11.22 | 5,575 MiB |

The two ridge rows use identical requests and training membership. Without the
multiplier, size-only underestimates 194/369 and richer 196/369; maximum
overprediction rises from 2.27 to 10.67 times actual memory. On the 348 requests
predicted by every model, both multiplied ridge models have zero underestimates,
but that intersection excludes the difficult extrapolations. It does not
establish safety on the full workload.

One GHC compilation used 6,120 MiB versus predictions of 468 MiB from size-only
and 546 MiB from richer features, including the multiplier. A Swift package
used 2,242 MiB versus 257 and 274 MiB respectively. The largest GHC compilation
used 7,535 MiB with only four completed prior observations: both regressions
abstained, while the prior maximum predicted 336 MiB. First large requests can
arrive while relevant earlier compilations are still running.

The nearby-size envelope requires two prior inputs within a factor of two in
size and scales their maximum memory conservatively. It abstains on all ten
inputs of at least 6 MiB, including the three cheap Java compilations. Its lower
underprediction rate therefore does not demonstrate relief of the large-input
serialization cost that motivated the experiment. It predicts 376/611 requests;
the ridge models predict 369/611. Ordinary missing history remains significant.

Feature collection and hashing total 5.86 seconds over the recorded invocations;
the offline comparison takes 0.48 seconds. A separate 20-pair no-op probe measures
a 44.17 ms median added cost for the temporary Python observer, including its
startup. These are distinct boundaries, not an isolated whole-gate slowdown.
Linux `wait4` reports a maximum child/process RSS, not simultaneous process-tree
memory; no result establishes aggregate-memory admission safety or portability
of memory estimates to another platform.

Decision: do not adopt these predictors or the production receipt collector.
The richer features do not establish a useful improvement on the dangerous
cases, and the cautious alternative does not cover the relevant large jobs.
The collector repairs evidence attribution but does not itself improve
scheduling; this result does not justify its lasting maintenance cost. Preserve
the experiment and measurements without changing live admission. This rejects
the tested candidates, not all within-run learning approaches.

## Standalone Cargo compiler-only admission

Source: `ci/cargo.sh`.

Recorded result: in a counterbalanced `ABBA` measurement using fresh equal-shaped targets, an actual independent test-runner build overlapped the golden reduced-Prime prebuild. Total wall time fell from 36.862/36.855 seconds with the former barrier admission to 29.333/29.609 seconds with compiler-only admission (20.0% by arm medians); a waiting one-slot work marker fell from about 36.626 seconds to 0.055/0.045 seconds. Summed process-tree CPU was 70.83–71.56 seconds with the former admission and 68.63–68.92 seconds with compiler-only admission; trial-wide aggregate peak RSS was not measured. A sampled broad gate with the same seeds and `--jobs=8 --compiler-jobs=2` completed 53/53 tasks in 3,512 seconds versus a retained 4,482-second baseline; because sampled implementation assignments can differ, this broad result is integration and critical-tail evidence rather than the causal comparison. Rust tests in `resource_admission`, `held_resources`, and `compiler_admission` pin concurrent resource policy, canonical nesting order, inherited-lease retention, and scheduler re-entry; `schedule-entry-selftest.sh` pins only shell-facade dispatch, state discovery, argument/stdin transport, explicit bypass, and Cargo opt-in plumbing.

## Persistent corpus-tool Cargo targets

Source: `ci/checks/orchestrators/lib/common.sh`.

Recorded result: on exact compiler base `e04c9ccc5ba7be34efa24e510b313d30258f367b`, four cold-target arms compared four concurrent former private-target builds with four concurrent production helper calls in counterbalanced `A1/B1/B2/A2` order. Every arm used four work slots, two fixed compiler producers, default Cargo incremental behavior, no whole-Cargo serialization variable, and the same Prime-only `kio-prime` command. No compiler wrapper was configured and every arm recorded a zero sccache-stat delta. Median wall fell from 83.530 to 40.490 seconds, saving 43.040 seconds (51.526%); both adjacent pairs improved by 51.633% and 51.423%. Median user-plus-system CPU fell from 317.405 to 83.460 seconds, saving 233.945 seconds (73.706%); median maximum RSS fell from 2,799,768 to 2,772,182 KiB. At each cold arm's end before cleanup, median allocated Cargo-target storage fell from 6,088,085,504 bytes across four control targets to 1,521,948,672 bytes in the persistent candidate target (75.001%), while all 16 private copies were 203,201,304 bytes with SHA-256 `aaffe40b188befa1223c4576d9083ded5b7b1f970557f99caa58d4c4f415c3af` and every caller exited zero. Candidate arms also recorded negligible memory pressure, no live swap-out, and no cgroup OOM-counter change; control and candidate receipts include per-second CPU/memory/I/O telemetry. The measured production diff had SHA-256 `4e0a0305bbe60da5e7fb3d5778d6b43c185144c334fbbdee0f6d565e1e28c3eb`; the later input-validation-only rejection of exact `.` and `..` keys does not fire for the measured `kio-prime` key and leaves its target, build, and copy path unchanged. This focused build-and-copy measurement gives no broad-gate credit.

## Bounded-pass `run-tests.sh` worklist construction and shared package preflight

Source: `ci/run-tests.sh`.

Recorded result: on the same 1,089-golden fake-compiler fixture, setup before worker
  dispatch fell from 33.26s to 10.87s for one implementation, from 171.85s to
  6.29s for `SAMPLE_IMPL` over eight implementations, and from 95.93s to 4.85s
  for the exact eight-implementation matrix. Under matched process tracing,
  process-creation calls fell from 22,349 to 1,161 (94.8%). A follow-up made
  target indexing consume only already-filtered cases: on a 1,095-case,
  1,857-manifest, 43,661-file golden tree, an anchored exact-filter zero-unit
  run fell from a median 11.095s to 10.585s wall and from 11.760s to 11.225s CPU
  across eight counterbalanced warm samples per arm (4.6%); the corresponding
  full-corpus target-index microstep changed from 0.297s to 0.355s per pass, so
  a more complicated hybrid path was not justified. The shared-inventory
  measurement compared against the earlier case-marker traversal's one
  subprocess per case. Four warm `ABBA BAAB` samples over the natural
  1,218-case / 1,194-package golden tree measured the old case traversal plus
  sort at median 7.195 seconds wall and 7.165 seconds user+system CPU, versus
  0.310 and 0.305 seconds for shared discovery, classification, and case
  sorting (95.7% lower for both); median peak RSS changed from 8,316 to 8,492
  KiB (+2.1%). The shared discovery emitted 319,511 bytes. This microstep
  excludes the subsequent per-selected-case workdir/sentinel probes, bounded
  awk join, and manifest target parsing and gives no broad-gate credit; the
  retained timing table has SHA-256
  `c5f9ba006f6bf7d4a72a7d81c862a7ee625f121926257e05cbbfc29072efc207`.
  A matched production-path check then compared exact base `feab3d91b` with
  candidate `2418c810e` over the same full corpus while selecting one exact
  standard case and one fake compile-only implementation. After one warmup per
  arm, four `ABBA BAAB` samples measured the complete harness invocation at
  median 8.690 seconds wall and 8.825 seconds user+system CPU for the old path,
  versus 1.965 and 2.055 seconds for the candidate (77.4% and 76.7% lower).
  Median peak RSS changed from 46,466 to 46,566 KiB (+0.2%), and all eight
  output logs were byte-identical. This setup-and-one-no-op-unit measurement
  gives no compiler, runner, or broad-gate credit; its timing table has SHA-256
  `548fc766ec0871ca74a1de208dd256e21bcc545b51395f03a5c4c41fc8ef0dc3`.
  `run-tests-predispatch-selftest.sh` pins the preserved worklist contracts,
  the exact missing/symlink/nested/multiple/workdir-symlink package
  classifications, pre-dispatch enforcement for a compile-only standard row,
  generated-descendant/out/target/hidden pruning, discovery-root marker
  non-pruning, standard package-identity transport, custom and test-only
  exemptions, and the exact two corpus/final-marker `find` processes.

## Exact-name selector batching

Source: `ci/run-tests.sh`.

Recorded result: on a 1,089-case fixture with 32 anchored selectors and the sole hit last, four measured samples per arm after one discarded batched warmup in counterbalanced `ABBA BAAB` order produced median wall times of 88.070 seconds for the legacy loop and 6.285 seconds for the batch path (92.9% lower), with `min(legacy)=87.518` greater than `max(batch)=6.441`. Matcher invocations fell from 34,848 legacy comparisons to one batch process. Every arm had byte-identical stdout, stderr, status, and selected-case output, and tripwire tools proved that no case compiler or runner work executed. Scaling only the separated lower bound and pessimistically debiting all measured batch time as pattern-scan growth gives a conservative 341-selector saving of 801.774 seconds after a 62.198-second debit. Unfiltered runs bypass this optimization, so the measurement gives no broad-gate credit. `run-tests-predispatch-selftest.sh` pins exact-path/order parity, safe-classifier firing and no-op behavior, general-regex fallback, clean no-match handling, diagnostic/partial-output fallback, Prime/dynamic/exclusion interactions, bounded matcher processes, path forms, and mutation-live causals.

## Golden Kio' roundtrip multi-target batching

Source: `ci/checks/per-case/kio-prime-roundtrip.sh`.

Recorded result (deterministic operation counts): on the fixed cohort of 32 regular, 3 Prime, and 4 dyn-load-prime executions, its 35 regular + Prime roundtrip rows reduced producer compiler transactions from 70 to 35 and total roundtrip compiler invocations from 105 to 70, including unchanged reduced builds. This evidence makes no wall-time, CPU, RSS, latency, or O(1) claim. `ci/checks/repo-lint/kio-prime-roundtrip-batching-selftest.sh` is the causal firing guard; it pins the combined request, fail-closed fallback, inapplicable-target skip, failure attribution, independent verifier, and byte-for-byte comparison.

## Exact public bridge scope for scripted dynamic-load hosts

Source: `test-data/goldens/00_success/exec_dyn_load_*`.

The original one-case discharge used exact base `34b723e3939db0bf17d46c0864b9a168ae2d7d18`: three runner-cache-disabled control trials were interleaved with a conservative narrowed candidate before three immediately following trials of the final exact-eight `exec_dyn_load_exact_descriptors` manifest. Every control and exact-final trial ran that one anchored Haskell golden with the same compiler binaries, GHC `-O0`, and the native compiler observer; all six passed the runtime golden and invariants, every control emitted SHA-256 `959623fabe30e07d16876ef803bc0154ab9b5439254888beb3a0aa64b5b42169`, and every exact-final trial emitted SHA-256 `30a0aeefd22bd426ccc8fb36c9a60edd3b57b40b13784e5c42c0ce2f94ef1902`. Median whole-case wall fell from 532.98 to 205.63 seconds (61.419%), and median summed user-plus-system CPU fell from 753.29 to 415.28 seconds (44.871%). Median GHC wall fell from 385.83 to 64.61 seconds (83.254%), median GHC CPU from 387.81 to 64.87 seconds (83.273%), and median peak RSS from 5,953,156 to 3,506,336 KiB (41.101%); every compiler reported zero swaps. Generated Haskell fell from 41,445 lines / 21,961,471 bytes to 6,779 lines / 10,101,357 bytes. The control and exact-final manifest SHA-256 values are `9df613fd5fd39ff8bf4a546c910029b19dbae6ec8585a877c0cf3d25c752d6ad` and `0b75030a7c1c5a1c810d553eb4372640a2182dd5f261d0a38f699e99aa96f9f3`.

  Generalization discharge: on exact base `c13bdb6c1`, an anchored runner-cache-disabled Haskell comparison covered `exec_dyn_load_exact_descriptors`, `exec_dyn_load_host_callback`, and `exec_dyn_load_type_exports` with `--jobs=3`, `--compiler-jobs=2`, and GHC `-O0`. The control arm, with `exec_dyn_load_exact_descriptors` already exact-eight and the other two manifests still overbroad, measured 461.59 seconds wall, 1,237.52 seconds summed user-plus-system CPU, and 5,853,500 KiB peak RSS. After one discarded candidate warmup, the arm with all three manifests exact-eight measured 267.13 seconds wall, 745.92 seconds CPU, and 3,632,116 KiB peak RSS: reductions of 42.128%, 39.725%, and 37.950%, respectively. Every runtime and invariant row passed in both measured arms. These focused measurements give no broad-gate credit. `dyn-load-host-surface.sh` derives the complete scripted-host cohort structurally, validates its canonical source and fixed protocol fail-closed, and enforces the exact root bridge surface; its self-test is the causal firing guard. The unchanged fixed runner remains the independent sufficiency oracle, and the custom typed-cache cohort independently snapshots its reviewed 26-script subset.

## Phase-local dynamic-Prime driver preparation

Source: [ci/checks/orchestrators/golden-tests.sh](../ci/checks/orchestrators/golden-tests.sh).

A retained ordinary-gate trace
  measured 265.75 seconds of FIFO compiler-admission wait for this preparation
  before any regular case ran. Deferral removes that dependency from regular
  and direct-Prime readiness, not necessarily from the whole-gate critical path.
  The existing `coverage-policy-selftest.sh` fixture records regular, Prime,
  driver, dynamic order; restoring eager preparation fails that guard.
  Selected-group controls, a driver exit 17 that prevents dynamic execution,
  and an earlier phase exit 29 that survives later successful phases pin
  selection and failure behavior. No extra scheduler, cache, worker or phase
  protocol is introduced; total gate time remains a separate observation.

## Case-marker shell directory extraction inside shared discovery

Source: [ci/run-tests.sh](../ci/run-tests.sh).

Recorded result: on exact base `feab3d91bc26e2dc7b0282510f76abc2418e3fb5`
  and the natural 1,218-case golden tree, four matched samples per arm in
  counterbalanced `ABBA BAAB` order produced byte-identical sorted case paths.
  Median wall time fell from 7.050 to 0.165 seconds (97.660%, 42.7x), with the
  slowest candidate at 0.18 seconds below the fastest baseline at 6.60 seconds.
  Median user-plus-system CPU fell from 7.065 to 0.155 seconds (97.806%); median
  maximum RSS rose from 8,030 to 8,180 KiB (1.868%). The change removes 1,218
  one-file `dirname` subprocesses while retaining the same `find` and `sort`
  passes. The measured production patch has SHA-256
  `c884ac66ef714b01c7db5c4a31d4fecb3aea296757d9a097d839d8cab59c9405`.
  Measurements of earlier discarded designs are excluded from these measurements.

## Batched Prime-marker source discovery

Source: [ci/checks/per-case/prime-marker.sh](../ci/checks/per-case/prime-marker.sh).

The discovery-only comparison uses the workdirs
  of `exec_dict_fold_list_interop`, `root_module_header_minimal` and
  `build_intrinsics`, in that order under `test-data/goldens/00_success/`.
  Baseline commit is `3504053d874b07cd92c1db700b9a0a39f87eeb5d`; measured
  production-script SHA256 is
  `8a7b52745a7bcd7c766e5109f04492f1062e86f1119f4fc3d28218c52d44c205`.
  With one warmup per arm and four counterbalanced `ABBA` samples, each sample
  repeats those three workdirs 16 times (48 discovery transactions). Median
  wall was 10.270 versus 2.065 seconds (79.89% lower), user-plus-system CPU
  10.835 versus 2.290 seconds, and peak RSS 11,904 versus 11,938 KiB. Every
  sample returned zero with byte-identical ordered candidate lists and empty
  stderr. These are exact discovery-prefix timings, not compiler, verifier,
  whole-check or broad-gate savings.

## Package-only optimization of CI compiler snapshots

Source: [ci/checks/orchestrators/lib/common.sh](../ci/checks/orchestrators/lib/common.sh).

Initial opt0→opt1 discharge: on source
  `1d2f3f1a0378da6d84b23fca3abf38e62a5b05dd`, one ordered
  matched A/B over the exact Java `exec_dyn_load_exact_descriptors`,
  `exec_dyn_load_curried_signature` and `exec_tuple_projection_effect_order`
  cases reduced whole-harness wall from 175.37 to 111.29 seconds (36.5%).
  Cold loader build, owned-equiv test and receiving-warm build changed from
  69.79/65.24/27.25 to 43.55/40.59/14.43 seconds; cache keys, generated-shape
  counters and all runtime/invariant results matched. All three native
  compiler calls ran in each arm, taking 4.69 versus 4.62 seconds in total.
  The ordinary `circuit_netlist_eval` Java control passed in 11.43 versus
  8.43 seconds. These ordered pairs include order and instrumentation effects;
  they are bounded measured wins, not confidence-bounded speedup estimates.
  Initial Full preparation cost 136.93 seconds with warm dependencies, and
  reduced preparation was approximately 73 seconds. A real warm leaf-edit
  rebuild increased from 21.14 to 40.87 seconds, so preparation and rebuild
  costs must remain part of the comparison. The adopted ordinary
  `ci/all.sh SAMPLE_IMPL` gate took 1,994.24 seconds; charging both initial
  preparations gives approximately 2,204.17 seconds versus a retained
  3,427.55-second warm baseline. Different case/backend draws prevent a causal
  whole-gate speedup claim, and warm dependencies prevent a cold-checkout claim.
  The opt1→opt2 comparison on source
  `f6c91e38554dd27235e360801d3c93e25f7d767a` used one ordered, counter-free
  exact Python `exec_dyn_load_exact_descriptors` harness pair with all eight
  per-case checks. Wall changed from 84.05 to 54.09 seconds (35.65% lower),
  user-plus-system CPU from 212.79 to 109.63 seconds; both runtime and
  invariant rows passed, with identical source, inputs and observer binaries.
  Each arm used a fresh ordinary authenticated loader cohort and the same
  host-runner cache. There is no memory-saving claim. Full/reduced preparation
  totaled 120.70 seconds for restored opt1 artifacts versus 221.75 seconds
  for new opt2 artifacts, with warm dependencies; those are different initial
  states, not a matched cold-build premium. An identical real warm leaf-edit
  rebuild took 74.43 versus 79.97 seconds, and 224.84 versus 347.23 CPU seconds.
  That bounded edit does not represent every compiler change. Preparation,
  restoration (71.33 seconds) and rebuild costs remain separate from the
  execution win;
  this pair does not establish a whole-gate speedup or amortization across
  the broad corpus's warm cohorts.

## Authenticated dynamic-loader typed-cache cohort

The regular golden harness shares a run-scoped typed-module cache among the
scripts authenticated by `ci/checks/orchestrators/custom-typed-cache/exec-dyn-load-goldens.tsv`.
Fifteen additional loader scripts use private temporary package trees, invoke
ordinary compiler commands and the fixed host runner, and do not manipulate the
shared cache. Their exact script object IDs extend the existing authenticated
cohort; the harness still snapshots and validates every listed script before
running it.

An ordered comparison selected twelve loader cases on the JS runner: four
already in the cohort (`alias_imports`, `type_exports`, `wrapped_params`,
`diagnostics`) and eight newly added (`applied_literal_alias`,
`arrow_result_parentheses`, `binding_origins`, `bool_specialization`,
`boolean_operands`, `conditional_types`, `identity_alias_stages`,
`named_import_records`); each name has the `00_success/exec_dyn_load_` prefix.
Each arm invoked `ci/run-tests.sh` with the canonical goldens directory, a
single `kio@js` implementation using the fixed JS runner, the existing custom
cohort option, and anchored filters for those twelve names. The baseline used
the original 26-entry manifest; the candidate added the eight selected IDs.
No case-binary checks were supplied to this isolated runtime comparison.
Both arms used the same frozen
opt2 compiler from source `ac7977d7e48e78ddb899a40c65d5ebe725300681`, the same case inputs and fixed host runner,
default eight-worker dispatch and adaptive compiler admission, and a fresh
run-scoped typed cache. All twelve runtime cases passed in each arm. The
existing cohort took 135.57 seconds wall and 907.65 seconds user plus system
CPU; the expanded cohort took 99.51 seconds wall and 665.03 seconds CPU.
Peak RSS was 2,054,292 and 2,023,536 KiB respectively. This single ordered pair
includes normal operating-system cache and order effects; it measures the
selected workload, not a whole-gate speedup or every host backend. The compiler
and JS runner SHA-256 hashes were `37449bb61c20d8b9534e7621040bf3729f01b5068cf475e8e8df0b4684d687b4`
and `5b659f12afa61123a3f500f83ccee4461a607b6634ce1c667c288d4f5ef23227`.

## Website documentation uses the prepared compiler

The website orchestrator uses the existing full-feature compiler snapshot for
example validation and documentation generation. The snapshot retains package-only
opt-level 2, debug assertions and overflow checks, with Cargo fingerprints as
freshness authority. Its lifetime belongs to the website task, and direct Kio
calls retain compiler admission. Standalone website builds keep their ordinary
Cargo route when `KIO_BIN` is absent.

One ordered pair ran `kio --no-cache doc build --html` from `docs/` through
compiler admission, using compiler source
`ac7977d7e48e78ddb899a40c65d5ebe725300681` and documentation from
`a02e2c5ea3be99e35bd95106c460a64fe62b0a26`. The existing ordinary developer
binary took 318.30 seconds wall, 1,641.71 seconds user plus system CPU, and
2,777,084 KiB peak RSS. The existing opt2 snapshot took 72.79 seconds wall,
336.28 seconds CPU, and 2,642,108 KiB peak RSS. Both completed successfully.
Their SHA-256 hashes were
`8fdd5c0c79143c4317e71ea08a842a2cc37ca7cbb693e16bcaa84d99213f5cd8` and
`37449bb61c20d8b9534e7621040bf3729f01b5068cf475e8e8df0b4684d687b4`.

This measures compiler execution on the whole documentation package, with
semantic caches disabled and no competing build. It excludes compiler
preparation and the rest of the website build; the broad gate already prepares
the same opt2 tool key for other consumers. Operating-system caches were not
reset, and the single ordered pair does not establish a causal whole-gate
speedup. Generated HTML was not retained separately between arms, so this pair
does not establish byte equality.

`prepare-docs.test.mjs` checks both invocation routes, argument boundaries,
admission and failure propagation. `lsp-executable-snapshot-selftest.sh` checks
the website's exact compiler preparation and private lifetime, including output
replacement and cleanup after success or failure.

## Ordinary sampled gate after the typed-cache and website changes

Source `6b290a63e363f3f3ad329910c07de6a302cf21ff` passed the ordinary
`ci/all.sh SAMPLE_IMPL` gate: 91/91 groups, 2,176 seconds wall. The run used
implementation seed `667989904`, case seed `1794848241`, generator seed
`2201226477171244282`, and default adaptive resources. Regular goldens passed
1,776 cases and 1,776 invariants; direct Prime passed 463 and 463; sampled
dynamic Prime passed 50 cases and 661 invariants. Golden tests took 2,173
seconds wall and 7,287.1 seconds task CPU, website took 761 and 874.8, and
`kio-rs` hygiene took 982 and 2,329.7. The gate did not meet the 30-minute
objective. This successful run provides an observed reference, not a causal
comparison with earlier gates: tool and cache states, samples and overlapping
work differ. The task CPU figures omit work delegated to a shared compiler-cache
daemon.

## Package-only optimization of CI hygiene tests

The all-features hygiene test command uses the Cargo test profile with
package-only `kio-lang` opt-level 2 and explicit debug assertions and overflow
checks. Ordinary developer tests and dependency profiles remain unchanged.
`lsp-executable-snapshot-selftest.sh` pins the full hygiene argv and the
ordinary LSP test argv as a negative control. Against the original hygiene
command without these overrides, the unchanged expectation failed with exit 90
and identified `test --all-features`; the updated command passed.

On source `6b290a63e363f3f3ad329910c07de6a302cf21ff`, frozen all-features
libtest binaries with matching lists of 6,628 tests each ran once, unfiltered,
with default test threads, identical pinned full toolchains and fixed compiler
snapshots. Both passed 6,623 tests and ignored five. The ordinary profile took
343.99 seconds wall and 2,060.40 seconds process-tree CPU; opt-level 2 took
81.10 and 503.26 seconds. Their peak RSS was 1,070,060 and 994,600 KiB. A
third opt-level 1 binary passed the same tests in 149.68 seconds wall and
914.85 seconds CPU, at 1,019,800 KiB peak RSS. This ordered single-sample
screen excludes the build and does not establish a whole-gate gain.

With warm dependencies, the first opt2 libtest build took 241.95 seconds wall,
peaked at 10,164,872 KiB RSS and grew the workspace target by 4,766,412 KiB.
The opt1 build took 220.02 seconds, peaked at 9,533,384 KiB, and added
4,584,580 KiB after the opt2 artifacts were already present. A matched
non-semantic leaf edit rebuilt the ordinary test binary in 29.43 seconds and
4,088,708 KiB peak RSS, versus 98.18 seconds and 8,312,396 KiB for opt2. The
source was restored byte-for-byte. These are ordered, warm-worktree samples;
compiler-cache daemon CPU can escape process-tree accounting, peak RSS is not
an aggregate concurrent-memory bound, and no cold build was measured.

The full focused hygiene check then passed in 452.93 seconds wall, with
2,091.50 seconds process-tree CPU and 8,116,148 KiB maximum child RSS. Its
all-features test preparation took 3m21s and the library suite took 78.22
seconds; all 20 Cargo test-result groups matched the preceding gate's counts,
with 7,064 passed and five ignored in total. The workspace target grew by
8,231,204 KiB during this complete check. It included compiler snapshot
rebuilds after restoration of the temporary edit and was run without competing
builds, unlike the preceding broad gate. These conditions prevent treating its
wall time as a matched broad-gate comparison; the extra preparation, disk use
and concurrent-memory risk remain material.

The ordinary `ci/all.sh SAMPLE_IMPL` gate on
`7580341d2c29e2fb9525933f3b71e0d3b16d8e36` passed all 91 groups in
1,987 seconds wall (33m07s). It used the same three seeds and default adaptive
resources as the preceding gate on `6b290a63e`; the 2,900 implementation
assignments and 2,900 baseline assignments matched exactly. Regular goldens
passed 1,776 cases and 1,776 invariants, direct Prime passed 463 and 463, and
sampled dynamic Prime passed 50 and 661. Golden tests took 1,985 seconds wall
and 7,351.5 seconds task CPU, hygiene took 622 and 698.4, and website took
638 and 970.5. No competing build or performance probe ran. The observed gate
was 189 seconds shorter, but retained caches and scheduling differences prevent
attributing that whole difference to the test-profile change. The 30-minute
objective remains unmet.

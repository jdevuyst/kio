# Local performance

Pointer: read when choosing how to run local checks or builds efficiently, managing one or more worktrees, or coordinating parallel local sessions.

This page is advisory except for § Delegated and background work lifecycle,
which implements the mandatory ownership rule in `AGENTS.md`, and the
repository-mutation context rule in § Worktree hygiene. The remaining sections
describe what tends to work locally; none of them changes the check contracts in
[`local-ci.md`](local-ci.md) or the tool setup mechanics in
[`local-tools.md`](local-tools.md).

## What costs locally

A broad `ci/all.sh` pass is expensive because it combines a `kio-rs` build with every check bucket. Rust-heavy checks, especially `checks/hygiene/kio-rs.sh`, are dominated by `kio-rs`; the support crates under `ci/infra/` are comparatively light.

For language/runtime codegen changes or golden edits, prefer focused golden filters with the coverage required by [`local-ci.md`](local-ci.md) § Scoping a test pass. For generated-host ABI or durable artifact-shape changes, use an exact backend-first emission selector instead. That keeps backend-divergence coverage where it matters without paying for unrelated cases; emissions still supplement and never replace runtime goldens.

## Machine shape and contention

On constrained CPU or memory, start with the scoped checks named in [`local-ci.md`](local-ci.md) before launching a broad gate. Focused corpus commands and `ci/all.sh` share Git-common work admission; standalone Cargo does not consume work slots, while top-level Cargo invocations, compiler-producing Kio commands, and actual native compiler commands share best-effort adaptive admission. Its aggregate feedback reduces new admissions under observed memory pressure but cannot bound an arbitrary command's allocation burst. A deliberately lower fixed compiler cap can constrain concurrent producers; `CARGO_BUILD_JOBS` separately constrains one Cargo invocation's internal fan-out. Neither is an ordinary default recommendation or a universal memory guarantee.

On many-core machines, use the default work fan-out and automatic compiler feedback. Use the summary's total CPU vs. wall time and the per-task CPU/wall rows to investigate oversubscription: stretched wall time with modest CPU can reflect queueing, locks, I/O, memory pressure, or compiler work charged to a shared daemon. Distinguish work-unit contention (`--jobs`), cross-compiler capacity (`--compiler-jobs`), and one Cargo invocation's internal fan-out (`CARGO_BUILD_JOBS`) before changing the tests themselves. A measured numeric compiler value is an explicit fixed override, not a replacement for the ordinary adaptive path.

Measure the ordinary broad-gate performance target with both scheduler overrides
omitted. A run with a numeric `--jobs` or `--compiler-jobs` value is a useful
controlled diagnostic, but it cannot establish the performance of the default
scheduler path.

`SAMPLE_IMPL` normally chooses a fresh random applicable implementation for
each case. When a same-tree or A/B performance comparison needs that
implementation assignment held constant, set
`KIO_DEBUG_SAMPLE_IMPL_SEED=<0..4294967295>` on every compared invocation of
`sh ci/all.sh SAMPLE_IMPL`. Use canonical unsigned decimal (for example `0`,
not `00`). The seed keys each choice by corpus, regular/direct-Prime/dynamic-
Prime phase, and relative case name; every selected mapping is printed as a
`sample-impl-map:` line in the owning task log. With `--keep-logs`, compare
those lines exactly before crediting a timing result. Mapping equality proves
only the implementation assignment. Every arm must also use the same explicit
`--case-seed`, `--gen-seed`, generated-case count, and case-coverage policy;
retain and compare the printed selected-case blocks and the `kio-gen:`
seed/count/Prime-mode record to prove the executed workload matches. The
variable is an internal performance-replay probe, not ordinary coverage
policy: it is rejected outside `SAMPLE_IMPL`, and omitting it preserves the
normal random selection and output. It does not justify a numeric scheduler
override.

When `sccache` is the compiler wrapper, read those per-task CPU rows with one caveat: the measurement counts only a task's own process-tree CPU, and `sccache` hands each compile to a long-lived shared daemon outside that tree. A compile-bound task — one that builds many crates, such as the golden suite's emitted-Rust cases — then reports high wall time with near-zero CPU, which is indistinguishable in the rows from a task waiting on a lock, I/O, or memory. Check whether the task is actually compiling before reading its low CPU as contention; the daemon's compile cost is real, only attributed elsewhere. Without `sccache`, `rustc` runs inside the task tree and that CPU is counted normally. The shared daemon also means per-task compile CPU cannot be cleanly attributed under a concurrent fan-out — only in a fully serial run.

## One worktree

Cargo's worktree-local `target/` cache is the main accelerator for repeated Rust checks in one worktree. `cargo build` and `cargo test` share dependency artifacts at the default `dev` profile, and a populated `target/` lets Cargo skip most local crate rebuilds and link work on reruns.

For Rust-heavy editing sessions, keep shared binaries warm during editing with:

```sh
sh ci/watch-builds.sh
```

The watcher debounces source edits, cancels stale in-flight builds, and reruns only `cargo build` for `kio-rs`, producing the `kio` and `kio-prime` debug binaries consumed by the local corpus checks. It intentionally does not run tests, clippy, format checks, golden tests, generative tests, or support-crate builds. If a final `ci/all.sh` starts while the watcher is active, Cargo's own internal locks keep its shared package state correct; set `KIO_CI_SERIALIZE_CARGO=1` only when local CPU or memory pressure makes whole-Cargo serialization preferable.

Run the watcher with the same Cargo cache environment as the final gate. If a session uses `sccache` for `ci/all.sh`, use the same `RUSTC_WRAPPER` / `SCCACHE_DIR` setup for `ci/watch-builds.sh`.

For one-time warmup, prefer `ci/watch-builds.sh` or a narrow build-only command that matches the final check's Rust binaries. A broad hygiene script is a poor warmup because it runs format, clippy, tests, and feature combinations before the gate runs them again.

`sccache` is still useful, but it does not replace `target/`: a warm sccache with an empty `target/` still has to rebuild and link local crate artifacts, test harnesses, and feature configurations. For repeated checks in one warm worktree, do not force `CARGO_INCREMENTAL=0` unless measurement says it helps; that setting favors cross-worktree sccache reuse over Cargo's local incremental rebuilds. Clear `target/` only for disk pressure or a specific stale-artifact investigation.

If a launched `cargo` build, test, or `ci/` script becomes redundant, kill the specific process you launched by PID. Letting an obsolete run finish burns local CPU and memory, and blanket name-matches such as `pkill -f cargo` can terminate unrelated work in another worktree.

## Cache visibility

Before a broad Rust-heavy local run, make the cache state explicit using [`local-tools.md`](local-tools.md) § Compiler cache. If the wrappers are empty despite `sccache` being available, say so before starting a broad run; that makes the cold-build tradeoff visible.

Rust runner-cache misses are content-key misses. Before changing runner cache policy or aggregation strategy, compare the emitted `out/rust/src/lib.rs` and adjacent support files across repeated runs. The focused, harness-owned guard for this class is `sh ci/checks/orchestrators/rust-output-determinism.sh`, which builds a small Rust-output slice twice and compares the emitted `lib.rs` hashes. A case-owned generated-file assertion belongs in an `ARTIFACT_SHAPE` emission only when a backend spec or recorded measurement makes that exact fact durable; private deterministic-emission invariants belong in unit or mutation coverage. If only alpha-renamable synthetic locals change, fix deterministic compiler emission at the source; broadening the runner cache would mask the instability. If emitted sources are stable but warm runner reruns still invoke `rustc`, count cache-miss compiles with `KIO_TEST_RUNNER_COMPILER_WRAPPER` before changing architecture. Treat aggregate runner compilation as a separate cold-compile optimization candidate, not as a substitute for stable generated bytes.

## Failure handling

Once any bucket fails, the run is red. Live `FAIL [...]` markers from parallel harness workers are early signal; the deterministic buffered logs and failed-task diagnostics at the end remain the canonical detail. Letting the remaining tasks finish is useful when they can produce independent signal for the same investigation; stopping early is reasonable when the rest of the run would only consume scarce CPU or memory. When stopping, terminate the process tree you launched rather than broad process-name matches, so sibling worktrees and unrelated local sessions keep running.

Judge that tradeoff from the run's purpose, using the exact fail-fast boundary
in [`local-ci.md`](local-ci.md) § Stop-the-line on failures. A diagnostic or
first-discovery run can profit from collecting every independent failure. For
an exact-tree correctness checkpoint expected to pass, an early deterministic
failure whose repair makes a replacement broad run inevitable usually makes
the remaining stale-tree corpus redundant. A performance run remains useful
only when the failure does not invalidate the path being measured, and it does
not become correctness clearance.

## Delegated and background work lifecycle

Delegating work transfers execution, not ownership. The coordinating agent keeps
a live record for every delegated or background task: its objective, owner or
process, worktree and durable checkpoint, current state, expected completion
event, and the action that follows success or failure.

An authority wait is scoped to the action that needs the unanswered decision.
Record that blocked action and the decision or event that releases it, then
continue work that does not prejudge the answer: read-only investigation,
validation of an already-frozen authorized change, and other independently
authorized tasks. Do not implement both sides speculatively, edit normative
artifacts to manufacture authority, or merge a decision-dependent branch; do
not turn that narrow prohibition into an idle session either.

Use event-driven waits for agent mailboxes and launched processes. When an event
arrives, consume it before waiting again: inspect the result, surface failures,
checkpoint or merge cleared work, assign any required response or fresh review,
and update the live work record. When no event arrives, run a coordination sweep
at least hourly over agent state, processes and logs, free capacity, worktree
cleanliness, and scratchpad accuracy. Do not replace that sweep with a rapid
wait loop or repetitive no-change progress messages.

Before sending a terminal response, reconcile the complete live record against
the actual agent and process lists. Every launched task must be completed and
its result acted on, explicitly canceled, or unable to progress without named
user or external input. While work remains able to advance, give informational
updates as progress messages and keep the coordination turn alive. A status
request can legitimately expose a missed transition and should trigger an
immediate correction, but polling by the user is never the ordinary mechanism
for discovering completion.

## Debug probes

Kio-owned debug, profiling, and timing-only switches use `kio debug ...`, `KIO_DEBUG_...` environment variables, or `--debug-...` command-line arguments. Keep their option documentation in this internal guidance layer or implementation-local comments, not in public `specs/` or `docs/`.

`KIO_DEBUG_TIMING=<list>` enables timing output for comma-, space-, or semicolon-separated categories. Accepted category tokens are `frontend`, `build`, `lsp`, `eval`, `derive`, and `all` (also `1`, `true`, or `yes`). Category-specific aliases exist when a single boolean switch is easier to pass through a tool: `KIO_DEBUG_FRONTEND_TIMING=1`, `KIO_DEBUG_BUILD_TIMING=1`, `KIO_DEBUG_LSP_TIMING=1`, `KIO_DEBUG_EVAL_TIMING=1`, and `KIO_DEBUG_DERIVE_MEMO=1`.

Cache and memo probes are deliberately opt-in because they write extra stderr:

- `KIO_DEBUG_SAMPLE_IMPL_SEED=<0..4294967295>` — deterministically replay the
  per-case implementation assignments of `SAMPLE_IMPL` for matched performance
  comparisons; see § Machine shape and contention for mapping verification.
- `KIO_DEBUG_PACKAGE_CHECK_CACHE=1` — package-check cache hit/write status.
- `KIO_DEBUG_TYPED_CACHE=1` — typed-module cache hit/miss status.
- `KIO_DEBUG_TYPED_CACHE_ROOT=<absolute-path>` — route typed-module entries for
  packages that explicitly enable caching to one diagnostic cache root; it
  never enables caching for `cache ();` or `--no-cache`. Empty and relative
  paths are ignored.
- `KIO_DEBUG_ENRICHED_CACHE=1` — enriched-IR cache hit/miss status and compute timing.
- `KIO_DEBUG_EMIT_CACHE=1` and `KIO_DEBUG_ARTIFACT_CACHE=1` — backend output cache status.
- `KIO_DEBUG_DOC_CACHE=1` — Kiodoc snippet cache hit/miss status.
- `KIO_DEBUG_EQUIV_CACHE=1` — `kio test` equiv cache hit/miss status.
- `KIO_DEBUG_MEMO=1` — in-memory typecheck memo trace lines.
- `KIO_DEBUG_MEMO_VERIFY=1` — compare memoized elaborator/typecheck results with fresh computation and panic on divergence.
- `KIO_DEBUG_WRITE_TYPED_CACHE=0` — disable typed-module cache writes while investigating cache behavior.
- `KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER=<executable>` — place one opaque
  executable outermost around every actual Rust, Go, Java, Haskell, or Swift
  test-runner native compile. This is an invocation observer, not a compiler
  cache wrapper: it remains active when the runner artifact cache is disabled,
  does not wrap identity probes or runtime launches, and does not fire for warm
  hits or same-key waiters. The observer receives the former command as its
  argv without shell splitting. See the runner
  [README](../../ci/infra/kio-test-runner-rs/README.md#runner-build-cache-and-compiler-wrappers)
  for exact nesting and readiness semantics.
- `KIO_DEBUG_CI_SCHEDULER_TRACE=<run-token>` — write one versioned JSONL file
  per independent compiler acquisition below the Git-common scheduler root at
  `debug/compiler-admission/<run-token>/`. The portable token accepts 1–64
  ASCII letters, digits, `-`, or `_` and excludes reserved filenames. Schema
  `kio-ci-compiler-admission-v3` records exact queue state and typed blockers
  without changing admission policy; repeated waits are recorded only on
  semantic transitions. The `adaptive-cap` blocker identifies the effective
  adaptive bound. A non-head or fixed-only evaluation does not sample feedback;
  its `adaptive_capacity` is null rather than an invented current target.
  Program attribution is an opaque hex-encoded basename, never a tool
  classification. Disabled scheduling and inherited compiler-lease reuse create
  no file. Traces persist until explicitly removed; after the run exits, inspect
  or remove them at
  `<git-common-dir>/kio-ci-schedule/debug/compiler-admission/<run-token>/` (or
  below the explicit `KIO_CI_SCHEDULE_DIR`). Setup or write failure fails the
  affected acquisition so an incomplete trace is not silently accepted.

### Reading frontend timing

Use `KIO_DEBUG_TIMING=frontend` when a `kio check` or `kio build` change may have moved frontend, cache, or user-elaborator cost. Compare at least these cache states before drawing conclusions:

- `--no-cache` — pure recomputation baseline, with semantic cache reads and writes disabled.
- cache-cold — delete `.kio-cache/`, run normally, and treat extra time as cache population cost.
- repeated elaborator shapes in one run — force multiple modules or calls with the same shape to exercise the in-process template and prepared memos independently of persistent-cache state.
- fully warm — run normally twice; package-check hits should make elaborator/typecheck counters drop to zero for skipped packages.

Interpret the lines in this order:

- `frontend-workspace-timing` explains work before per-package typechecking: walk/parse, source-map assembly, package-check cache-state construction, typed-cache state construction, and package-level layout. If a warm run is slow while package lines show `package_check_skipped=1`, look here first.
- `frontend-timing` is per package. `package_check_lookup_ms`, `typed_cache_lookup_ms`, and `typed_cache_store_ms` explain non-elaborator cache overhead. `prepare_ms`, `forced_modules`, `typed_hits`, and `typed_misses` explain how much module work was still needed. `typecheck_ms` is the aggregate of `pipeline_typecheck_ms` and `prime_validation_ms`. The pipeline stage includes elaborator work when modules are forced; it checks Lowered input in `kio` and source-Prime input in `kio-prime`. The validation stage is the standalone check of the assembled Prime artifact in both binaries.
- `frontend-package-cache-store-timing` reports post-green package-check cache writes. It appears after package work because package-check entries are written only after the workspace succeeds.
- The `user_elaborator_*` fields explain user-elaborator cost. For repeated shapes in one run, `user_elaborator_prepared_hits`, the template batch unique/duplicate counts, and the `KIO_DEBUG_MEMO=1` trace show in-process reuse; `user_elaborator_template_eval_ms` measures fresh template evaluation while `user_elaborator_template_replay_ms` measures applying trusted in-process templates at each call site. `user_elaborator_eval_arg_vecs` / `user_elaborator_eval_arg_values` and `user_elaborator_eval_capture_vecs` / `user_elaborator_eval_capture_values` are allocation-pressure counters for the compiled evaluator's hot value-vector sites. In a fully warm package-check hit, all user-elaborator counters should be zero.

When comparing built-in and user elaborators, use a fair corpus where both variants parse/typecheck the same imported elaborator modules unless the experiment is explicitly about dependency weight. Report medians and include cache state; a single cache-cold wall time is usually measuring cache writes, not steady-state compiler speed.

### Reading build timing

Use `KIO_DEBUG_TIMING=build` when a `kio build` regression is larger or differently shaped than the matching `kio check` regression. Combine it with `frontend` when you need both the typecheck portion and the post-typecheck backend portion:

```sh
KIO_DEBUG_TIMING=frontend,build kio build js
```

Interpret the lines in this order:

- `build-workspace-timing` reports the build command's reused typecheck phase, parallel target-dispatch wall time, selected target count, and total command wall time.
- `build-target-timing` reports one build target. For artifact-cache hits, only artifact restore and total time are meaningful. For misses, JS reports artifact restore, target-dir prep, Prime shape logging, Enriched recovery/optimization, Routed lowering, capability annotation, Routed shape logging, backend emit, artifact writes, host-shape writes, and artifact-cache store. Rust and Kio' report coarser backend-specific slices.
- `build-shape` reports debug-only AST shape counters at Prime, Enriched, and Routed boundaries where that backend reaches them. Use these to tell whether two builds emit identical final code but carry different intermediate trees through structural recovery, lowering, or backend optimization.

Shape logging walks the AST only when build timing is enabled. Do not leave it enabled during benchmark runs unless you are intentionally measuring or explaining the build-phase structure.

## Multiple worktrees

Sibling worktrees do not share `target/`, so the first cargo invocation in a fresh worktree pays local artifact construction even when sccache is warm. For a sequence of related Rust work, one long-lived warm worktree usually beats repeatedly creating disposable worktrees.

Sibling worktrees *do* share the test-runner **artifact** cache: the orchestrators default it to a machine-stable shared root, and because the key is path-normalized and the cached binaries are path-neutral, a golden / castle / POC binary compiled in one worktree is reused by another without recompiling. So for the backends (not the `kio-rs` toolchain build itself), a fresh worktree starts warm against whatever a sibling already built. See [`local-tools.md`](local-tools.md) § Compiler cache for the location and the size-LRU bound; clearing that shared cache is machine-wide.

Concurrent Cargo across sibling worktrees is correct. Top-level `ci/cargo.sh`
commands share the Git-common compiler resource with compiler-producing Kio
commands and native runner compilers. Adaptive feedback adjusts new admissions
within live CPU and explicit fixed ceilings; fixed-only activity ignores the
feedback record; see the [policy](../../ci/infra/kio-ci-scheduler-rs/README.md#omitted-compiler-capacity).
Warm runner-artifact hits take no permit; Cargo admission covers the whole top-level
invocation, including an incremental no-op. A newly started lower
`--compiler-jobs` request takes effect while active producers drain. If
additional commands deliberately bypass this protocol, several cold `kio-rs`
builds can still exhaust memory in a constrained local environment. Options:

- Let one cold `kio-rs` build finish before starting another.
- Use `sccache` so cacheable rustc work shared across worktrees becomes fast reads.
- Prefer the default shared compiler cap before serializing whole Cargo invocations; it bounds admitted Cargo/native-compiler work while runner-cache hits and non-compiler work continue.
- Prefer one broad `ci/all.sh SAMPLE_IMPL` gate at a time when broad runs contend on more than compiler memory; its scheduler leaves useful non-Cargo work parallel.
- Set `KIO_CI_SERIALIZE_CARGO=1` as described in [`local-tools.md`](local-tools.md) § Scheduler-native Cargo serialization when multiple launched commands or sibling worktrees should serialize whole Cargo invocations.
- Set `CARGO_BUILD_JOBS=1` when a single heavy build's internal parallelism strains the machine. This caps one build; it does not coordinate across separate cargo processes.

Cargo serialization is best reserved for cold builds. Serializing incremental rebuilds can turn the capacity-one resource into the bottleneck.

### Fanning out building agents

Launching N background agents can still produce N build requests, but top-level
repository Cargo commands through `ci/cargo.sh`, compiler-producing Kio
commands in the test harness, and native runner compiles coordinate
automatically through the shared compiler resource. Size its capacity from
measurement rather than discovering the ceiling through the OS out-of-memory
killer:

- With ample memory and cores, keep the conservative adaptive compiler admission unless measurement justifies an explicit fixed `--compiler-jobs` value; concurrent cross-worktree builds are correct, and warm runner-cache hits consume no permit.
- On a memory-constrained machine, lower `--compiler-jobs` for participating commands before serializing whole Cargo invocations. An OOM-killed `rustc` surfaces as a spurious build or golden failure, not a compiler finding; re-verify it, and never reshape a golden to dodge it.
- Direct compiler commands and `KIO_CI_SCHEDULE=DISABLE` are outside this protection. Use them only at an intentional boundary, and account for their memory alongside admitted producers.

The optional `cargo` resource remains the stronger option: it serializes whole
Cargo invocations, so one agent compiles while the rest block even when the
compiler resource would admit another. Use it only when Cargo-wide
serialization itself is required; otherwise the compiler resource preserves
more overlap by admitting non-compiler work and warm hits.

Watch for a **scheduler resource retained by a lazily spawned daemon**. The
exact symptom is platform-specific:

- **Unix inherited descriptor.** A daemon first spawned inside admitted work inherits the compiler lease descriptor. The scheduled leader can exit and its invocation can return, but later compiler-producing commands stop entering because the daemon still makes the claim live.
- **Windows trapped Job member.** A daemon first spawned inside an ordinary admitted Job remains a member of that Job. The scheduler preserves the leader's status but deliberately waits for the complete Job to drain, so the original invocation stays blocked after the compiler leader exits.
- **Prevention on both.** The shell, runner, and broad-gate entry points perform an early fail-fast probe and recheck an explicitly configured sccache through the generic readiness hook after any admission wait. That hook closes Unix lease descriptors or uses Windows Job breakaway before the explicit adapter runs. A wrapper configured through an uninspected Cargo config still relies on the required session-level check.
- **Inherited progress descriptors are not a `ci/all.sh` completion dependency.** The progress reporter stops on an explicit sentinel rather than FIFO EOF, so a long-lived descendant cannot hold the final summary open merely by inheriting the FIFO. Keep that sentinel contract if the progress transport changes; compiler-lease inheritance is the remaining liveness hazard addressed here.

If a daemon has retained a Unix lease or kept a Windows Job nonempty,
`sccache --stop-server` releases that resource. Do that only with no build in
flight, then export `SCCACHE_IDLE_TIMEOUT=0` and use
`sccache --dist-status >/dev/null` outside any build to start it cleanly. Normal
prevention is the same session-level check plus the repository entry points'
early and generic post-admission readiness hook; see
[`local-tools.md`](local-tools.md) § Scheduler-native Cargo serialization.

Estimate peak as roughly one cold `kio-rs` rustc's resident size per concurrently building agent; compare that against available memory to choose between scheduler-native whole-Cargo serialization and the default or an explicitly measured fixed compiler capacity.

## Per-session worktrees

Default to one worktree per active session or feature stream, not one worktree per small topic or follow-up. Keep the session worktree under the configured sibling-worktree area, reuse it for related edits and focused checks, and run the final `ci/all.sh` gate from that same worktree. Reusing the worktree keeps its `target/` cache warm across the inevitable follow-up edits that happen after review or user feedback.

When the session genuinely ends and its branch has been fast-forwarded into `main`, it is fine to remove the session worktree. Do not remove and recreate a worktree between related follow-ups just because a subtask was merged; that discards the exact cache state the next gate needs.

## Multiple local sessions

When several local sessions can work on disjoint files, let each run the scoped checks its change could break, then merge finished branches into an integration worktree off current `main` and run the final local gate once there. For Rust-heavy integration work, running `sh ci/watch-builds.sh` in that integration worktree while resolving and reviewing keeps the `kio-rs` build artifact warm without turning the warmer itself into a gate. That catches interactions between individually green branches before `main` moves while keeping the final gate's `target/` cache warm.

Fan out only disjoint file sets. Several sessions rewriting the same large file hand the integration worktree a textual merge problem, and the heavy Rust build cannot truly run in parallel across those edits anyway. Sequence shared-file work on a single accumulating branch in a warm worktree.

Skip the integration-worktree pattern for a single session, or when branches are trivially non-interacting such as disjoint single-file documentation edits.

## Worktree hygiene

### Disk headroom

Before a broad run or another build-heavy worktree, check free space on the
actual checkout and temporary filesystems. Allow room for expected build growth,
not just the current command. Under pressure, measure the largest directories
before choosing what to remove; free-space totals alone do not identify the cause.

Inspect completed, no-longer-needed worktrees first. Preserve reachable commits,
uncommitted and untracked work, and required evidence; confirm no process still
uses the checkout before removing it through Git. Retain a useful warm worktree
for related follow-ups rather than repeatedly paying for cold rebuilds.

Also inspect temporary storage outside registered worktrees: abandoned prototype
targets and frozen compiler or test executables can outlive their investigations.
A temporary path or old timestamp alone is not deletion authority. Remove only
identified, unused rebuildable outputs within the authorized cleanup scope,
preserving source and required logs. For inactive binaries still needed as
evidence, consider lossless compression: record the original path, checksum,
size, executable mode and restoration command, and verify a complete round trip
before removing the original. Do not transform artifacts still in use.

Use the [cache index](caches.md) and the
[`clear-caches`](../skills/clear-caches/SKILL.md) process preflight for cache
removal. Keep shared caches a separate deliberate choice, not routine cleanup.
Recheck actual free space afterward and report what was removed, what can be
restored or rebuilt, and whether sufficient headroom remains.

### Editing context

For tracked-file changes, a sibling git worktree keeps `main` clean while local checks run. Two exceptions are normally fine:

- Edits confined to `scratchpad/` belong on the main checkout because `scratchpad/` is gitignored and each worktree has its own copy.
- A change that touches at most one tracked non-code file can be done directly when there is no cross-file consistency to verify and no code gate to run. If it grows beyond that, move it to a worktree before committing.

When working in a worktree, read and edit files under the worktree root. A file read from the main checkout for reference should still be edited at the worktree path. Keep session worktrees in the configured sibling-worktree area, reuse them for related follow-ups, and name each branch after the session or feature stream it carries rather than the date or each tiny follow-up.

`git worktree add` creates a checkout but does not change the shell's current working directory. Run every later repository mutation with explicit target context: either set the command's working directory to the worktree root or use `git -C <worktree>` as the command prefix. Never chain worktree creation with an implicit-context `git cherry-pick`, `git commit`, or `git merge`; creating the worktree does not retarget later commands in the same shell invocation.

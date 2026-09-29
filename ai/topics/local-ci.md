# Local CI

Pointer: read before running Kio's check scripts locally, invoking `cargo` through local check infrastructure, or scoping a test pass for fast feedback.

This page explains how Kio's local check infrastructure works. It does not cover third-party tool wiring; see [`local-tools.md`](local-tools.md). It does not prescribe local performance strategy; see [`local-performance.md`](local-performance.md).

## End-to-end sweeps

`sh ci/all.sh SAMPLE_IMPL` runs the same check categories as the Linux GitHub gate, with each matrix case using one applicable implementation. Each check is also invocable individually as `sh ci/checks/<bucket>/<name>.sh` (see [`ai/topics/repo-layout.md`](repo-layout.md) for the `ci/` bucket structure).

`ci/all.sh` is intentionally discovery-based over the standard check buckets, not a hand-maintained scheduler list. Keep that shape when changing broad-gate reporting or scheduling: adding a new orchestrator, repo-lint check, or hygiene check in the bucket layout should make the broad gate pick it up without another edit. Leaf work is admitted by `ci/schedule.sh`: corpus `(case, binary)` units consume work slots, while top-level Cargo and other compiler-producing commands use the independent compiler resource. Early-error visibility belongs in compact progress/failure events, not in a bespoke scheduler list or full live log streaming: `ci/run-tests.sh` emits live `FAIL [...]` markers for completed failing parallel units, while `ci/all.sh` keeps final task logs buffered and deterministic.

`sh ci/all.sh FULL_IMPL_MATRIX` runs the full implementation matrix and is much heavier. When backend-divergence coverage matters, a focused golden run with `--impls=FULL_IMPL_MATRIX` is usually narrower than a repository-wide `ci/all.sh FULL_IMPL_MATRIX`.

GitHub CI uses two layers. Linux runs directly on the hosted Ubuntu runner, with mise installing the complete eight-backend tool pool from the shared root pins. Scheduled and default manual runs use `ci/all.sh SAMPLE_IMPL --sample-cases --impl-verification=dyn-load-prime --case-coverage=dyn-load-prime:10`: one applicable regular implementation per selected case, plus the separately selected direct-Prime and dynamic-Prime verifier rows. Case sampling is independent of implementation sampling. The dynamic differential's eligible cases all belong to `00_success`, so its existing seeded per-bucket cap of 10 selects 10 cases total; the eligibility lint rejects markers outside that bucket. `workflow_dispatch` exposes independent `implementation_coverage=full` and `case_coverage=full` opt-ins. Full implementation coverage changes the per-case multiplicity; full case coverage removes case narrowing, including the dynamic differential's 10-case cap. Neither changes local command defaults. Locally regular goldens, direct Prime, POC, and emissions default to all cases, while dynamic Prime samples 50 per bucket; see § Scoping a test pass.

The ordinary hosted runtime caps are 100 per top-level bucket for regular and direct-Prime goldens, 10 each for POCs, castles, and contrib, 10 total for the success-only dynamic-Prime differential, and one emission case per backend bucket. Every narrowed-out case retains its wired case-binary checks, and known-failing reproducers retain their implementation gate. Generation is a separate axis: the generative job still creates 200 programs and runs each on one applicable implementation; `--sample-cases` does not reduce its batch. Ordinary loader goldens and the loader POC remain under their owning corpus policy, not the dynamic verifier's sampled policy.

macOS / Windows run one sequential portability job per OS. Both prepare the scheduler, run its native self-test and crate tests, then select up to 100 seeded `00_success` case names for compile checks. Applicable `run.args` and `run.test-only` cases run `kio test` plus `kio build js` without running emitted output; `run.sh` and non-JS cases do not enter that compiler path. A manual `case_coverage=full` dispatch removes sampling but retains those applicability rules. The raw harness call wires no per-case check scripts, so narrowed-out cases do not run the compiler checks, though dependency fetch can still occur. Selected cases provide frontend/emitter compile evidence, not runtime backend coverage. macOS additionally runs the full kio-rs tests, Kiodoc and LSP checks, and a small castle sample across the JS / TS / Rust / Swift impls. Windows runs the kio-rs library tests and a smoke package through `kio check`, `kio test`, and an all-target `kio build`; emitted runtime output, QuickJS-binding backend runners, and POSIX-only integration tests do not run there. The generative orchestrator logs the random seed it used, so a generated-case failure can be reproduced by rerunning with that seed. Each portability job keeps one checkout and one Cargo target directory, so repeated script-level Cargo builds still get Cargo's stale-artifact checks while reusing the same debug binaries when inputs are unchanged.

Scheduled runs and a manual dispatch with default `platforms=all` start Linux, macOS and Windows. A manual `platforms=linux`, `macos`, or `windows` dispatch starts only the selected platform job; `platforms=portability` starts macOS and Windows without the Linux gate. The portability matrix is selected before runner allocation, so a Windows-only retry does not allocate macOS or Linux runners. `implementation_coverage` applies to Linux whenever selected; `case_coverage` applies to Linux and the portability golden compile checks. The workflow concurrency key includes the selected platform set, so a Windows-only retry does not cancel a Linux-only run on the same ref. Different selections can overlap on a platform (`all` and `windows`, for example); avoid concurrent overlapping selections unless that duplicated work is intentional. A new dispatch with the same selection and ref still cancels its prior in-progress run.

Both hosted implementation modes install all eight backends because
`SAMPLE_IMPL` samples the configured pool; it cannot be combined with an
explicit implementation list. `ci/impl-toolchain.sh tooling-impls` also closes
any requested tool list over ambient compilers used by availability-driven
checks, including compile-only `javac`. Its executable-shim detection does
not grant runtime implementation eligibility or change case coverage.

Tool requirements match what the root [`mise.toml`](../../mise.toml) / [`mise.lock`](../../mise.lock) pins. [`.devcontainer/Dockerfile`](../../.devcontainer/Dockerfile) installs the core set; `ci/impl-toolchain.sh` maps implementation names to Linux CI backend-extra mise tools and `mise bootstrap packages` specs; macOS / Windows portability jobs install their direct runner tools with `jdx/mise-action` from the same root mise files, excluding platform/tool cells that lack a proven no-source-build route.

Cargo invocations in `ci/` scripts build with the default `dev` profile (output under `target/debug/`). `cargo build` and `cargo test` share dependency artifacts at that profile, so the orchestrator builds and the unit-test runs don't pay for duplicate dep compilation. Reach for `--release` only when measuring or shipping a real artifact.

The native scheduler state lives under the Git common directory, so sibling
worktrees coordinate even when a focused corpus command or top-level
`ci/cargo.sh` invocation starts outside `ci/all.sh`. Default work capacity is
the host's `std::thread::available_parallelism`; omit `--jobs` in ordinary use.
Compiler capacity is adaptive unless `--compiler-jobs=<N>` supplies a fixed
hard cap. Omitted capacity uses shared, paced CPU/memory feedback rather than
a fixed producer count; see the scheduler's
[policy](../../ci/infra/kio-ci-scheduler-rs/README.md#omitted-compiler-capacity).
The same resource policy
and queue state machine run on Linux, macOS, and Windows. `ci/watch-builds.sh`
and direct check scripts reach the scheduler through their top-level Cargo
commands. Whole-Cargo serialization is the
optional scheduler-native `cargo` resource, enabled with
`KIO_CI_SERIALIZE_CARGO=1`; see [`local-tools.md`](local-tools.md) §
Scheduler-native Cargo serialization.

### Shared work, Cargo, and compiler admission

The scheduler has three independent resources:

- `work` admits corpus and other CI units. Standalone top-level Cargo does not
  consume this resource, so a queue of build requests cannot block fresh corpus
  admission. Cargo invoked by an already-scheduled worker retains that worker's
  existing work lease.
- `cargo` is a capacity-one, Git-common serialization resource for top-level
  repository Cargo commands. `KIO_CI_SERIALIZE_CARGO=1` opts ordinary
  top-level commands into it; the shared corpus-tool helper described below
  requests it explicitly regardless of that variable. Cargo subprocesses,
  emitted projects, and fixture crates remain direct and do not recursively
  reacquire repository resources.
- `compiler` caps admitted compiler-producing invocations across participating
  worktrees. Each compiler-producing Kio command and each native-runner,
  host-doc, or custom-script compiler command consumes one permit. A top-level
  Cargo invocation consumes one permit around Cargo as a whole; admission does
  not count or cap Cargo's individual `rustc` children.
  Omission selects conservative adaptive admission; `--compiler-jobs=<N>` is
  a fixed hard cap.

Resources are acquired only in `work -> cargo -> compiler` order; omitted
resources do not disturb that order. Every fixed client publishes its requested
cap. Admission uses the smallest fixed cap among active and pending clients, so
starting a run with a lower setting stops new admission immediately and lets
existing work drain to the smaller limit. An adaptive client publishes its
available CPU ceiling; feedback selects the target within live CPU and fixed
ceilings. A numeric fixed client retains its explicit positive capacity. This is why
`--jobs` and `--compiler-jobs` are capacities rather than private per-command
fan-out settings.

`ci/schedule.sh` is the single shell-facing scheduler facade. Its default and
`--barrier` forms map to `work`; `--resource cargo` and `--resource compiler`
map to the other native resources. The shell owns only path/shell adaptation,
standard-input topology, binary bootstrap, and applicable readiness-hook
registration. Queueing, locks, capacity policy, held-resource validation, and
process-tree supervision live in `kio-ci-scheduler`. There is no second shell
scheduler and no external lock utility.

The facade discovers `<git-common-dir>/kio-ci-schedule` when
`KIO_CI_SCHEDULE_DIR` is absent and exports a canonical absolute path before a
child can change directory. A nested command carries a canonical
`KIO_CI_SCHEDULE_HELD` subset and reuses resources its parent owns rather than
deadlocking on itself. Ordinary serial workers, including `--show-output` and
`--update-expected`, still enter `work`. A standalone or explicitly
`--jobs`-overridden `ci/run-tests.sh` publishes its resolved capacity. A
one-unit selection still uses serial dispatch locally but does not lower that
shared capacity; forced serial modes publish one. A nested invocation with no
local override preserves the outer `ci/all.sh` capacity. Streaming changes
output handling, not admission. Corpus orchestrators validate and export a
numeric `--compiler-jobs` override before any standalone Cargo prebuild. The
only explicit bypass spelling is `KIO_CI_SCHEDULE=DISABLE`, which bypasses all
three resources. On Windows this shell facade and its bootstrap require the
POSIX `sh` plus `cygpath` environment described in
[`local-tools.md`](local-tools.md) § Script portability; the scheduler binary
they resolve is native Windows code.

Broad and standalone entry points resolve `ci/schedule.sh --prepare` once
before fan-out and export the resulting absolute binary path. The bootstrap
keys scheduler sources, lockfile, applicable Cargo configuration, Rust host and
toolchain, Cargo identity, and output-affecting build settings. It publishes an
immutable binary below the Git common directory and keeps the Cargo target for
that exact key beside it. Compiler commands on the hot path only validate and
execute the inherited binary; they do not hash sources or invoke Cargo.

At the start of `ci/all.sh`, that binary runs a fast native `self-test` and
reports default work capacity through its `available-parallelism` query before
the gate fans out. The self-test uses isolated scheduler state and performs no
Cargo build. macOS and Windows CI execute the same native self-test and crate
tests, so their scheduler paths are not exercised only by a Linux-hosted
cross-check.

Corpus invocations use the scheduler's resource-free `--supervise` mode even
when resource admission is disabled. The outer `ci/run-tests.sh` process owns
its temporary root and requests cancellation through a private file; the
native supervisor then terminates and drains the complete Unix process group or
Windows Job. It atomically publishes a private drained marker only after that
tree is extinct. The shell preserves caught `HUP`, `INT`, and `TERM` statuses as
129, 130, and 143, and removes the temporary root only when the marker exists;
otherwise it preserves the state rather than deleting files beneath a possible
survivor. A native monitor failure remains the reported primary error but still
enters bounded `TERM`-then-`KILL` cleanup; an unproven drain publishes no marker
and does not take the terminal away from a possible survivor. Help is detected
with the ordinary option grammar before scheduler bootstrap, including after
accepted options. These control paths are not exposed to corpus cases. On
Windows the shell facade converts them with `cygpath`, as it does other paths
consumed by the native scheduler.

On Unix, the asynchronous handler records a process-group abort; the monitor
loop performs group delivery (`HUP`, `TERM`, or `INT` mapped to `TERM`). This
keeps asynchronous delivery from targeting a group identifier after the final
extinction observation while retaining exact 129, 130, and 143 statuses.

On Unix, if the supervisor's stdin is a controlling terminal and its process
group is the current foreground owner, the child rechecks that ownership and
takes the foreground in its pre-exec hook. The hook resets `HUP`, `INT`, and
`TERM` to their default dispositions before the transfer. Once the complete
child group is extinct, the supervisor restores the original foreground group
only if the terminal still belongs to that child; an exec failure restores an
extinct handoff without overwriting a different live owner. Because a terminal
signal goes directly to the foreground child group after this handoff, a child
leader status of 129, 130, or 143 starts the same `TERM`-then-`KILL` drain used
for an explicit cancellation. Numeric signal-shaped statuses do not trigger
that inference without a foreground-terminal handoff. The supervisor does not
implement a shell job-control proxy for stop signals such as `TSTP`; its abort
contract covers `HUP`, `INT`, and `TERM`.

The scheduler binary and native runners use the same Rust admission engine and
disk protocol for every resource:

```text
<KIO_CI_SCHEDULE_DIR>/<resource>/
  state.lock
  claims/
    <immutable-id>.lease
    <immutable-id>.pending | <immutable-id>.active
```

The identifier carries protocol version, live ticket, claim kind, and requested
capacity; the locked lease itself is zero length. Pending and active state use
separate markers so the scheduler never needs to read, truncate, or rename a
file while another process holds it locked. Activation creates `active` before
removing `pending`; if both survive an interruption, active wins. A scan under
`state.lock` proves liveness with a non-blocking lease-lock attempt and removes
only stale records. Unknown live protocol records fail closed. Tickets derive
from live claims rather than a persistent rewritten counter.

Admission belongs to the complete launched process tree. On Unix, descendants
inherit inventoried private lease descriptors, so killing only a runner or
shell supervisor does not release capacity while a compiler survives. The
Unix invocation itself waits only for the leader and returns its status; the
inherited descriptor keeps later admission blocked until the last descendant
closes it. A closed caller stdin remains genuinely closed. On Windows, the
scheduler creates the child suspended, assigns it to a
kill-on-close Job Object before user code runs, resumes it, and waits for the
Job's active-process count to reach zero. Assignment, resume, incompatible
nested-Job, or drain failures fail closed. The Kio Job allows explicit
breakaway only for the readiness hook; ordinary admitted commands do not ask
for it. This is cooperative resource control, not a security sandbox: a child
that deliberately requests breakaway can escape, and an enclosing host Job may
forbid breakaway. A closed caller stdin is mapped to `NUL`, preserving EOF
semantics without passing an invalid native handle.

The post-admission readiness hook is a generic explicit command vector. It runs
after any compiler queue wait and before target lease inheritance or Windows
Job creation, and a failure prevents the target launch. On Unix the hook closes
the validated lease-descriptor inventory, clears held-resource markers, and
starts in a distinct process group. On Windows the hook alone uses Job
breakaway. Both paths wait for the hook leader, allowing a daemon it starts to
outlive the probe without pinning an enclosing corpus invocation's drain. The
scheduler contains no sccache basename match. `ci/infra/sccache.sh` is the named
tooling adapter that registers this seam only for commands that can use the
configured wrapper, configures `SCCACHE_IDLE_TIMEOUT=0`, and owns the early and
post-wait probes. The semantic Kio compiler proxy supplies the generic private
`KIO_CI_SCHEDULE_READINESS=SKIP` proof because typechecking and source emission
cannot invoke the test runner's native compiler wrapper; the facade consumes
the marker before target launch. There is no persistent readiness marker: every
independently acquired sccache-backed compiler lease rechecks the daemon. A
nested compiler command carrying that same canonical inherited lease reuses
the readiness already established for the enclosing command, avoiding a shell
and daemon probe per compiler child. A wrapper hidden in Cargo configuration remains the reason
for the session-level check required by `AGENTS.md`; stopping or restarting the
daemon during a build violates the same lifecycle contract. With the pinned
sccache, `--dist-status` contacts or starts the daemon, while `--show-stats` is
not a readiness probe.

Policy and crash-transition tests are platform-neutral. Native Unix tests prove
descriptor inheritance and admission retention beyond leader exit. Native
Windows tests prove suspended assignment, cooperative cancellation,
supervisor-death cleanup, breakaway, and complete descendant draining; the
deterministic `scheduler_job_nests_inside_an_outer_job` test creates a real
enclosing Job and then exercises the scheduler's nested Job.
Cross-target compilation is compile evidence only; it cannot prove those kernel semantics
and is not credited as native evidence.

Adaptive compiler admission starts conservatively, grows with queued demand
and observed aggregate headroom, and reduces new admission under memory
pressure. Clients honor the minimum live CPU ceiling, feedback target, and
explicit fixed capacity. A numeric
`--compiler-jobs=<N>` remains an operator-selected fixed override. Feedback is
best effort, not a per-command memory reservation or an OOM guarantee. The
scheduler neither classifies compiler names nor suspends or terminates holders
in response to pressure.

The runner build cache receives compiler admission as an injected capability;
cache code does not read scheduler environment variables. After its per-key
lock and second cache probe, a miss hands the capability to the adapter; staging
and publication hold no permit. The adapter acquires immediately around each
actual compiler command, so Swift's two-step build releases between its two
`swiftc` calls. Warm hits and same-key waiters take no compiler slot.
Cache-disabled runner paths and direct/coexist compiler invocations acquire at
the same command boundary. The sole policy implementation lives in
`ci/infra/kio-ci-scheduler-rs/src/compiler_admission.rs`; the shell facade runs
its binary and the native runners depend on its library. Keep the facade,
library clients, disk protocol, and process-tree self-tests aligned.

The corpus harness routes `kio` and `kio-prime` through a static proxy after
resolving the configured binaries. Proxy metadata records executable paths as
text; only the small shell launcher is copied, avoiding per-case executable
copies from Git Bash's symlink emulation. On Windows, native drive and UNC paths
returned by command discovery are converted to shell paths with `cygpath`
before invocation-relative anchoring and proxy lookup. `fmt`, `cache`, `dep`, `init`,
`completions`, top-level help/version, and the exact internal
`debug kio-prime-roundtrip-package` helper remain direct. That helper only
parses and renders a copied manifest. Direct execution keeps terminal
inapplicable cases from retaining a work slot behind compiler capacity; an
applicable case avoids one admission overhead/FIFO turn before its immediately
following admitted build. Every other command, including other and unknown
`debug` commands, enters `ci/schedule.sh --resource compiler`, so an unknown or
newly added subcommand defaults to admission. Standard cases, case checks, and
custom `run.sh` files receive the same routed binaries. The harness's own cache
clear and dependency prefetch stay direct. Outside the harness, the
dyn-load-prime driver admits only a cache miss, while host-doc builds, Kiodoc
validation, and Rust-output determinism admit their compiler-producing Kio
commands directly.

Custom corpus `run.sh` files keep ordinary host-tool spellings. The harness
prefixes their inherited `PATH` with proxies for Kio, Kio', Cargo, rustc, Go,
javac, swiftc, and GHC after resolving the real tools. A custom script invokes
Kio through `KIO_BIN` or a bare proxy name; it preserves the prefix and invokes
host tools by bare command name. An absolute or previously captured real-tool
path bypasses compiler admission. Cargo, rustc, javac, swiftc, and GHC enter the
compiler resource through `ci/schedule.sh`, except the exact information-only
`rustc --version --verbose` query, which stays direct. Other rustc argument
vectors retain admission. Go admits
`build`/`install`/`run`/`test`/`vet`, `list` with `-export` or `-compiled`, and
`tool asm`/`cgo`/`compile`/`dist`/`link`; known non-compiling Go commands pass
through, while an unknown top-level subcommand, `go tool` command, or option is
admitted conservatively. A nested compiler inherits the held-resource marker
rather than reacquiring.
This is compiler-only admission: custom scripts do not enter `ci/cargo.sh`,
reacquire their existing work slot, or take the optional `cargo` resource.
Host-documentation snippet compiles call the same shell admission boundary
directly.

**An orchestrator must not mutate a tree another orchestrator reads.** `ci/all.sh` runs the buckets concurrently, so any working tree a check writes to — a Cargo `target/`, an npm workspace and its `node_modules`, a build output directory under `tools/` — is shared with whatever else is running. A check that installs or builds *in place* can pull the ground out from under a concurrent one, and the symptom is the worst kind: the mutating check passes on its own, the *other* check fails, and the failure looks unrelated to the change that caused it. Build into the check's own scratch directory instead, copying in what it needs. The rule below is the Cargo instance of this; it applies equally to `npm ci` (which deletes `node_modules` outright) and to any other in-place install step.

When an orchestrator builds a helper binary and then hands that binary to parallel workers, do not point the workers at Cargo-owned paths such as `target/debug/<tool>` if another `ci/all.sh` bucket may run `cargo build`, `cargo test`, or `cargo clippy` for the same crate. Cargo serializes Cargo commands, not arbitrary already-running worker executions of its output artifact. Copy the executable into an orchestrator-owned temporary path and pass that immutable snapshot to workers. The golden, emissions, POC, castle, and contrib orchestrators keep those snapshots and worker scratch under the ignored worktree-local `target/<orchestrator>.<unique>/`, export its `tmp/` child as `TMPDIR`, and remove the whole private root on exit.

CI consumers of the full compiler share a dedicated full-feature tool cache
and retain guarded executable snapshots for their own task lifetime. The
all-features Rust hygiene suite retains both binaries through
`KIO_DEBUG_TEST_KIO_BIN` and `KIO_DEBUG_TEST_KIO_PRIME_BIN`; consumers that
only invoke `kio` retain only that snapshot.
Every compiler subprocess integration harness uses the shared test-only
selection helper. Direct Cargo test invocations without an override use the
corresponding ordinary `CARGO_BIN_EXE_kio` or `CARGO_BIN_EXE_kio-prime` path.

The golden, POC, castle, and contrib orchestrators build their identical Prime-only compiler and grammar verifier through one shared helper. Normal calls use a dedicated cache below each owning Cargo workspace's existing `target/`; regardless of `KIO_CI_SERIALIZE_CARGO`, the helper explicitly takes the generic capacity-one `cargo` lease across the build and private copy, then `ci/cargo.sh` takes `compiler` in canonical order. Cargo fingerprints are the sole staleness authority. This policy is explicit at those four call sites rather than hidden in the scheduler. `KIO_CI_SCHEDULE=DISABLE` retains an invocation-private Cargo target because it also bypasses the lease. The default worktree-cache wipe removes the persistent targets with their owning workspace. The hosted macOS castle sample uses the same Unix path; the hosted Windows lane invokes none of these orchestrators, whose existing `target/debug/<name>` lookup does not provide or claim native `.exe` handling.

Do not run a broad hygiene script as a prewarm before `ci/all.sh`; that just serializes a full check before running the full gate. For Rust-heavy editing sessions, `sh ci/watch-builds.sh` is the optional continuous build warmer; when it pays and how to run it (same Cargo cache environment as the final gate) is advisory material in [`local-performance.md`](local-performance.md).

### A full run is expensive — scope its invocation, and do not reflex-kill it

`sh ci/all.sh FULL_IMPL_MATRIX` multiplies build and runtime work across the full implementation matrix. Two decisions about it deserve a deliberate pause rather than a reflex, because getting either wrong wastes a large block of CPU and wall-clock:

- **How to invoke it.** Before launching the repository-wide gate, decide whether the change actually needs it (§ When `ci/all.sh` is not required) and at what coverage (`SAMPLE_IMPL` vs `FULL_IMPL_MATRIX`, or a scoped orchestrator run per § Scoping a test pass). Defaulting to `FULL_IMPL_MATRIX` over a scoped run spends the most resources for the least marginal signal when the change surface is narrow — `FULL_IMPL_MATRIX` re-runs the whole matrix including orchestrators a localized change cannot affect.

  Count both axes before launch: selected cases and applicable implementations,
  including extra verification phases. Aggregate related commands, not just one
  invocation. Before an effectively corpus-wide full matrix, explain the
  uncovered risk, why the ordinary broad gate plus bounded representatives is
  insufficient, and obtain explicit user direction. Long lists of anchored
  selectors and partitioning them across commands do not make that work focused.
  An agent-authored checklist or a changed-file count does not authorize it.

- **Whether to kill one already in flight.** Default to *not* killing it. `ci/all.sh` fans out to long-lived children — the per-bucket orchestrators, their `ci/run-tests.sh` workers, and the `kio` build/test processes underneath. A *catchable* signal to the top-level process (`SIGTERM`/`SIGINT`/`SIGHUP`, including a terminal Ctrl-C) now tears that subtree down with it: each task runs in its own process group under `setsid`, and `ci/all.sh`'s abort handler reaps every recorded group, so the workers do not orphan to init. A `SIGKILL` (`kill -9`) cannot be trapped, so it still orphans the subtree — reserve it for a genuinely wedged top-level process, and tear down the whole tree yourself if you use it. Either way, killing rarely pays: an in-flight run almost always completes sooner than a kill-and-restart cycle would, and under `--keep-logs` its per-task results stay readable even if the top-level process is gone — so reading the logs is usually the answer, not a restart. Kill only for a concrete reason (a wedged/hung task — see the `0.0s cpu` hang signal in § Verifying the result). Beware that a bare `pkill -f <pattern>` can match the killing shell's own command line and take itself out before reaching the targets.

Abort cleanup accepts only canonical decimal process-group records from 2
through 2147483647, the supported signed process-identifier range. It ignores
malformed, zero, one, and overflowing records before either signal pass.
This validates the numeric operand; ownership still comes from the worker's
isolated launch. Self-tests that replace that launch use nonnumeric group
records, so cancellation cannot turn a fake identifier into a real target.
`ci-all-cancellation-selftest.sh` exercises the actual abort handler with
signal calls intercepted and checks the fake record independently.

## Local gate coverage

Compile-only implementations (`runner=SKIP`) execute applicable `run.args`
and `run.test-only` cases through the compiler-owned test/build path, without
calling a host runner. Custom `run.sh` cases remain excluded from those
implementations; target eligibility and implementation sampling still apply.

`UNKNOWN_BUILD_TARGET` routes a custom expected-build-error case independently
of its deliberately unknown target name. It requires `run.sh`, expected exit
40, and a nonempty declared target list; it does not bypass missing-build
validation. Unmarked cases retain their declared-target eligibility. Marked
cases retain implementation sampling, case narrowing and case-binary checks,
and remain excluded from compile-only `runner=SKIP` rows like other custom cases.

- **Integrated local state** — `ci/all.sh SAMPLE_IMPL` exercises every check category with one applicable implementation per matrix case.
- **Backend-sensitive codegen changes** — run the relevant runtime goldens against every affected backend and the exact affected emission cases. Goldens prove language/runtime behavior through opaque runners; emissions prove the public generated-host or durable artifact contract.
- **Dynamic-Prime coverage** — normally retain the broad gate's 50-case-per-bucket dynamic sample and add exact tests for the affected behavior. An isolated host-backend repair does not require every dynamic case. A new runner operation needs its exact dynamic regression and relevant existing-operation controls, not automatically the whole corpus. Require `--case-coverage=dyn-load-prime:all` when changes to loading, interpretation, or shared execution affect sufficiently broad behavior that exact tests plus sampling cannot adequately cover the risk. State the affected boundary and why narrower coverage is insufficient; neither the label "codegen change" nor a closeout gate supplies that argument. This does not relax the mandatory full-matrix, all-case coverage of each new or behaviorally edited golden.
- **Golden additions or behavioral edits** — run those specific golden cases with `--impls=FULL_IMPL_MATRIX --all-cases` and no conflicting global/root/leaf case policy or raw harness sampling override, unless a phase/CLI subject is inherently single-implementation. Behavioral edits change the case's tested acceptance or rejection, diagnostic subject, semantics, expected result, host/runner contract, or target applicability. Changed source bytes alone do not establish that classification: ordinary-source migrations can preserve the tested behavior while adopting authorized syntax. A syntax or formatter fixture whose asserted spelling changes is behavioral. Mechanical dependency refreshes and the reviewed migrations below use bounded exact full-matrix representatives plus normal harness coverage. A generated host-backend subject is an emission case, not an exception. Keep focused commands anchored and exact; widening to a bucket or corpus does not satisfy this rule.
- **Emission additions or changes** — run the exact `^<backend>/<case>$` selectors through `emissions-tests.sh --impls=FULL_IMPL_MATRIX`. Each case has one applicable backend, so this proves the selected compiler/backend row without widening the case axis. Emissions never satisfy a runtime backend-completeness cell; pair them with the applicable runtime golden evidence.
- **Iteration before the local gate** — a narrower scoped subset is fine. See § Scoping a test pass below for which knobs scope by what dimension.

An excessively expensive dynamic stress run may use the documented
[`SKIP_DYN_LOAD_PRIME` exception](../../TESTING.md#stress-and-semantic-coverage),
with bounded cost evidence, a reasoned marker and retained compact dynamic
semantic coverage. This is not a backend opt-out or a waiver for an interpreter
defect. The normal exact-case gate still covers every applicable row of the
stress case and its new or behaviorally edited semantic companions; it does
not require running a deliberately excluded dynamic stress row.

### Proof-carrying bulk corpus migrations

A large source diff neither weakens coverage nor automatically requires every
case on every backend. Separate new feature/regression fixtures from ordinary
cases that merely adopt authorized syntax or receive regenerated dependencies.
Their presence in the same batch does not make every adopter a behavioral edit.

Before applying a bulk transformation, record its rule and canonical-owner
inventory. Review evidence accounts for each changed owner and every derived
or symlink-reached occurrence. Classify rows as behavioral, migration-only,
shared-copy-only, or uncertain, with the applicable evidence. Edit canonical
owners and regenerate dependencies normally; copies are not independent sources.
An unchanged intended result alone is insufficient: the evidence must establish
that the transformation preserves what the case exercises.

Use evidence appropriate to the transformation, not a mandatory full-corpus
before/after compiler campaign:

- A deterministic syntax migration identifies the settled old/new mapping,
  preserves resolved providers and semantic argument slots, accounts for every
  transformed occurrence, and checks that unrelated bodies and case contracts
  remain unchanged. Reviewed transformation rules, per-owner applicability
  checks, inverse/structural comparisons and focused semantic controls can
  establish this; a text replacement count alone cannot.
- Incidental canonical reformatting retains formatter fixed-point evidence.
  A fixture whose spelling, comments or positions are themselves asserted is
  behavioral when that assertion changes.
- Inference-sensitive rewrites need semantic comparisons of the affected
  inferred types, resolved slots or validated backend-neutral artifacts; do not
  infer equivalence from similar text. Apply those comparisons to the relevant
  uncertainty, not automatically to every ordinary spelling migration.
- Expected-error migrations preserve the intended defect, diagnostic subject
  and exit category. Replacing the defect with a syntax failure is not
  equivalence. Examples teaching the changed syntax need content review, but
  being a teaching example alone does not imply backend behavior changed.

Migration-only rows retain expected results, stderr policies, runner contracts,
target applicability and intent. Manifest or harness edits need explicit
classification: a mechanical source spelling in a manifest can be migration-
only; changed metadata, routing, capabilities or oracles are behavioral.

Classify compiler/backend/codegen interactions per slice. Changes to parsing,
inference, lowering or execution that can alter a migration's meaning need
focused coverage of that interaction and broader affected-backend coverage
where justified. Merely sharing a commit or batch with compiler work does not
disqualify every migration row. Conversely, a genuine semantic/backend change
cannot be hidden inside a migration label.

Coverage has two parts:

1. Use each affected corpus's normal harness, with `SAMPLE_IMPL` for matrix
   coverage. The baseline `ci/all.sh SAMPLE_IMPL` runs ordinary goldens and
   direct Prime whole. Retain normal sampling for migration-only rows in
   sampled-by-default corpora/leaves, adding exact controls for each affected
   behavior or phase boundary. Broad dynamic-Prime coverage follows the
   risk-based rule above, not the count of mechanically touched sources.
2. Run anchored exact `FULL_IMPL_MATRIX --all-cases` for every new or behavioral
   golden and a bounded representative set for distinct migration risks:
   transformation, source/artifact kind, retained error path, positional
   boundary, runner route and backend applicability. Cover interactions that
   can affect behavior; do not multiply independent dimensions into a
   mechanical cross-product. Record why the chosen cases cover the risks and
   which existing results already discharge them. Conflicting case policies
   or raw harness sampling overrides must not prune these exact obligations.

Stacked migrations retain distinct claims and inventories, but share valid
evidence where their interactions are covered. Later changes invalidate the
evidence they affect, not unrelated completed cases. A format-only correction
can retain runtime evidence when review establishes token equivalence and the
subject is not formatting; the affected formatting check still runs again.

Uncertain rows remain unresolved until diagnosed; missing proof is neither a
waiver nor an automatic instruction to run the entire inventory on every
backend. Identify the uncertain transformation or interaction, inspect it and
run the smallest useful diagnostic. Actual behavioral edits receive exact full
coverage, and divergence broadens the implicated cohort. Preserve failing
tests and fix bugs rather than masking them. If the resulting proposal is
effectively corpus-wide full-matrix work, use the pre-launch checkpoint above.

### When `ci/all.sh` is not required

Use this table when the change is narrow enough that the affected check surface can be named directly. When the boundary is unclear, choose broader coverage.

| Change shape | Narrow coverage that usually gates it | Escalate when |
| --- | --- | --- |
| `test-data/` case source only | Run the relevant orchestrator with anchored exact case filters. New or behavioral goldens use `--impls=FULL_IMPL_MATRIX --all-cases` without conflicting case policies unless inherently single-implementation. Reviewed mechanical migrations use bounded full-matrix representatives plus normal harness coverage; see the classification and sampling rules above. Every emission uses its exact `<backend>/<case>` filter. For other corpora, choose impl coverage from the behavior under test. Resolve symlinked root modules when mapping affected cases. | Changed runner/expected-output contracts, backend applicability, shared harness assumptions or compiler behavior affect the slice; unresolved migration evidence or divergence requires diagnosis and proportionate broadening. |
| Prose-only `docs/`, `specs/`, or `ai/` guidance | Run the relevant markdown, skill, topic, or drift check for that area. | The prose changes a test contract, CLI contract, language behavior, or backend behavior; then run the checks for that contract too. |
| Repo configuration only | Run the specific `ci/checks/repo-lint/<name>.sh` bucket that owns the configuration. | The config changes how other checks execute, select files, or publish artifacts. |
| `kio-rs/`, harnesses, orchestrators, per-case checks, or `ci/infra/` runners | Prefer broader integration coverage; for harness and runner infrastructure, a scoped corpus run is rarely enough by itself. | Use `ci/all.sh SAMPLE_IMPL` plus exact affected-backend cases for implementation-sensitive behavior. Repository-wide full-matrix coverage requires the risk argument and explicit direction from the pre-launch checkpoint above. |

When adding or changing tracked `*.dep.kio` files under `test-data/`, including
fixture directories outside `workdir/`, also run
`sh ci/checks/repo-lint/dep-materialization.sh`; the focused case harness does
not replace this inventory check. It reads paths from `HEAD`, so validate the
candidate commit. Cold-fetch inputs stay non-`.dep.kio` templates until the
case instantiates ordinary declaration filenames in private scratch.
`SKIP_DEP_MATERIALIZED` remains reserved for intentionally non-materializable
dependencies; do not seed trees that the case requires to be absent.

Before focused or broad corpus checks of materialized dependencies, commit the
candidate's intended source and dependency changes on its feature branch.
`dep-canonical.sh` compares freshly fetched files against `HEAD`, not against
their pre-run contents; staged or unstaged migrations are not a clean baseline.
Regeneration prepares that baseline and does not replace its acceptance check.

Marked custom `run.sh` cases execute in a private whole-case copy per
implementation, retaining sibling path dependencies and all existing inputs,
including untracked and ignored files. Source checks and expected-result
updates still use the original case; each copy is made after those checks.
Cache clearing applies to the copy, and the supervisor removes it only after
the invocation's process tree drains. The harness never uses Git cleanliness
as evidence that it owns a source file. Copies preserve symlinks and relative
paths; fixture scripts remain responsible for any explicit external paths.

### Language and editor-tooling preflight

Use the changed behavior and its consumers to select the focused checks below.
These entry points cover different tooling boundaries; they are not a second
broad gate or a full-backend corpus run.

| Changed boundary | Required focused evidence |
| --- | --- |
| LSP behavior, or compiler syntax/semantics consumed by LSP | Relevant library tests plus `sh ci/checks/orchestrators/lsp-tests.sh`, which runs the separate `kio-rs/tests/lsp_smoke.rs` binary. A `--lib` pass does not run it. For source-generating code actions, apply the returned edit and analyse the resulting document; matching an edit string alone is insufficient. |
| Terminal REPL behavior or shared inspector logic | From `kio-rs/`, use affected library filters such as `sh ../ci/cargo.sh test --lib repl_core::` or `sh ../ci/cargo.sh test --lib repl::`, and `sh ../ci/cargo.sh test --test repl_smoke` for executable command/session behavior. The smoke target drives the real CLI through pipes; it bypasses interactive reedline completion and highlighting, which have separate library tests. A `--lib` pass does not run the smoke target. |
| Browser REPL wrapper or shared-core behavior consumed by it | `sh ci/checks/hygiene/kio-repl-wasm.sh` checks wrapper formatting, strict Clippy, native tests, a `wasm32-unknown-unknown` build, and exported JS/WASM methods and serialization in Node using the reduced `repl-core` feature set. It uses the mise-provisioned Node and wasm-pack; wasm-pack selects the matching wasm-bindgen runner. It does not execute the browser UI or validate its embedded package. |
| Shared token classification, highlighting, TextMate or tree-sitter grammar | `sh ci/checks/orchestrators/highlight-tokens.sh` and `sh ci/checks/orchestrators/highlight-agreement.sh`. If tree-sitter parsing changes, also run `tree-sitter test` from `tools/tree-sitter-kio/`; token agreement does not exercise the entire parse corpus. |
| VS Code behavior, semantic-token wiring, or bundled grammar artifacts | The extension's affected tests plus `sh ci/checks/orchestrators/vscode-e2e.sh` on a supported host. Grammar-source agreement does not validate the shipped editor bundle; report a platform/tool skip as missing runtime evidence, not a pass. |
| Syntax/semantics used by embedded fixtures outside `.kio` files | Inspect the affected Rust/JS/TS integration fixtures and their source generators. Run their owning targets; for a Rust integration target use `sh ../ci/cargo.sh test --all-features --test <target>` from `kio-rs/`. A `src/`-only inventory or `--lib` selection misses `tests/`. |

Choose only applicable rows and explain omissions from the changed boundary.
Preserve each fixture's intended semantic, diagnostic, location, and cache
assertions when updating syntax; do not turn an unrelated parse failure into its
new expected result. Keep independently useful passing evidence; broaden only
for actual interactions or uncovered risk.

REPL regression goldens supplement these Cargo targets. Select affected golden
cases through the ordinary case/implementation coverage rules; running the REPL
tests does not require a whole golden-corpus or full-backend-matrix pass.

### Check local `main` before the gate

Before invoking the final `ci/all.sh` gate for a worktree-to-`main` merge, compare the worktree branch against local `main`. Do not fetch `origin/main` as part of this local merge gate. The worktree branch should contain local `main` (`git merge-base --is-ancestor main HEAD`) and have commits that local `main` does not (`git rev-list --count main..HEAD`). Run the final gate only after that local relationship is clear, then fast-forward local `main` to the checked branch.

The check applies to the integration worktree described in [`local-performance.md`](local-performance.md) § Multiple local sessions, and to any other worktree about to run the final local gate. If local `main` is ahead of the worktree branch, first bring that local `main` state into the branch and rerun the gate. If the branch is not ahead of local `main`, there is nothing to fast-forward.

### No spurious deferrals before the gate

Before running `ci/all.sh` and before merging a worktree, audit your own work for **spurious deferrals**: bits of the assigned task left half-done, hidden behind a "TODO" comment, a vague "out of scope" note, or a follow-up file you intend to file later. Assigned work is finished regardless of size. The repo-wide [`AGENTS.md`](../../AGENTS.md) § Universal rules "No partial implementations" rule is the canonical statement; this section names the moment to check.

If a piece genuinely must defer — the host language fundamentally cannot express it, an unrelated tracked bug blocks it, the design surfaced a question the user must answer — surface it to the user and get explicit acknowledgment before merging. An agent does not silently defer; the user decides deferrals.

## Stop-the-line on failures

When `ci/all.sh` or any local CI invocation reports failing buckets, that's a blocker, not a data point.

- Enumerate the failing buckets at the top of the next response — not buried inside a status update or summary block.
- Don't queue more changes in the same area while CI is red; a follow-up commit can mask the regression and make bisection harder.
- Decide from the run's declared purpose. A diagnostic or first-discovery run that is not expected to pass may stay alive after a known failure because the remaining failures are its result. A correctness or delivery-checkpoint run expected to pass can no longer meet its purpose after a deterministic tree-changing fix becomes necessary. A performance run may remain useful only when the correctness failure does not invalidate the path being measured, and it never earns green-gate credit.
- If a `ci/all.sh` task fails while other tasks are still running, inspect that task's retained log immediately. Do not sit idle waiting for the final summary when the failing bucket's log is already available. Do not kill the broad gate merely because one task failed: sibling corpus or backend results may be needed for diagnosis, and a nearly complete run may cost less to finish than to replace. There is one important fail-fast case: when an early failure is deterministic and understood, its fix will change the tested tree, an exact-tree broad rerun is therefore unavoidable, and substantial expensive work remains, preserving the diagnostic and freeing those resources is a concrete reason to terminate the red run. Do not complete a stale-tree corpus and then pay for its inevitable replacement merely to follow the default keep-running rule. This judgment applies to actual task failure, not expected `FAIL [...]` output inside a causal self-test whose enclosing task passes.
- Keep the one-command workflow: do not manufacture a duplicate preflight suite that repeats broad-gate work before every `ci/all.sh`. Run the focused checks already required by the change before the broad gate; a Rust change, for example, should not reach the broad gate without its applicable strict Clippy or hygiene coverage. For changed shell scripts, run focused `shellcheck --shell=sh` on those files; `sh -n` checks syntax only and does not substitute. If the fail-fast case above occurs, fix and review the defect, rerun that focused coverage, then launch the replacement broad gate once.
- Continuing to edit while `ci/all.sh` runs is often fine, but do it deliberately: avoid mutating files an in-flight task may still read if you plan to trust that task's result, prefer isolated fixes whose affected bucket already failed or finished, and rerun the necessary focused or broad coverage after the edit. If you do stop the broad gate, say why; a catchable signal (`SIGTERM`/`SIGINT`/`SIGHUP`) to the top-level process reaps the worker subtree for you, so only a `SIGKILL` leaves a tree to tear down by hand.
- If failures predate the current session, still surface them ("X failed on the previous run; status now unknown — re-run or investigate?"). The user decides whether to act now, but with the information visible.

Treat unresolved local CI failures as a hard precondition on landing work in the affected area, the same way the [`AGENTS.md`](../../AGENTS.md) § Universal rules "no partial implementations" rule treats half-fixes.

## Verifying the result: don't lose the exit code — or the evidence

### Durable broad runs

Do not invoke `ci/all.sh` through `nohup`: its inherited ignored `SIGHUP`
disposition reaches task self-tests and makes their signal-trap evidence
invalid. Use the environment's external persistent session or job supervisor
(for example a terminal multiplexer, CI job, service manager, or agent process
supervisor), and run the gate in that supervisor's foreground. The repository
does not need its own detachment protocol.

Inside that supervisor, retain the full log and publish the real exit status
atomically. Choose a fresh persistent directory for each run:

```sh
CI_RUN_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/kio"
mkdir -p "$CI_RUN_ROOT"
RUN_DIR=$(mktemp -d "$CI_RUN_ROOT/ci-run.XXXXXX") || exit
(
  set +e
  sh ci/all.sh SAMPLE_IMPL --keep-logs >"$RUN_DIR/ci.log" 2>&1
  rc=$?
  printf '%s\n' "$rc" >"$RUN_DIR/exit.tmp"
  mv "$RUN_DIR/exit.tmp" "$RUN_DIR/exit"
  exit "$rc"
)
```

An absent `exit` file means active or interrupted, never success. Reconnect to
the external supervisor and inspect `ci.log`; do not start a duplicate run.

`sh ci/all.sh SAMPLE_IMPL 2>&1 | tail -n30` is a recurring failure mode, and it costs two separate things.

**The exit code.** The pipeline's exit status is `tail`'s (almost always 0), not `ci/all.sh`'s, so a failed run reads as success.

**The evidence.** A check's stdout is the *only* record of what it did: which implementations ran, which cases each one covered, how far it has got. None of it is reconstructable afterwards — the orchestrators keep no per-case log on disk, and their working directories are transient. Truncate the stream and the run becomes unreadable: a summary whose per-impl lines were cut looks exactly like a run where those implementations never executed, and a filtered stream leaves no way to answer "how far along is it?" short of running the whole thing again.

So: **never pipe a check's output through `head` / `tail` / `grep` as its primary capture.** Redirect the full stream to a file, then read whatever you want out of the file — including as many times as you need, with different filters. The same rule holds when backgrounding a long run: capture to a file, and read the file.

Two reliable patterns:

```sh
# Capture full output to a file; tail for display, exit on the real code.
log=$(mktemp); sh ci/all.sh SAMPLE_IMPL >"$log" 2>&1; rc=$?; tail -30 "$log"; exit $rc
```

```sh
# Enable pipefail before the pipeline.
set -o pipefail
sh ci/all.sh SAMPLE_IMPL 2>&1 | tail -30
```

The first form is preferable when invoking from a non-interactive driver — the explicit `rc=$?` makes the exit code visible and avoids relying on a shell-option side-effect.

**Read what ran, not just what passed.** `0 failed` is not a result; it is a result *given* some set of work. Before reporting a check as green, read its summary lines and confirm the implementations and case counts you expected are actually there. A run that executed nothing reports no failures, and so does a run whose output you truncated — the two are indistinguishable from a filtered log, and only one of them is good news.

The canonical CI signal is the **summary block**, not the exit code alone. The block ends with a `FAILED:` section enumerating failing buckets (when any failed) or only a `PASSED:` section (when all green). When parsing CI output, scan for `FAILED:` — its presence is the failure signal regardless of how the exit code propagated.

### Progress heartbeat: reading a long task while it runs

`ci/all.sh` buffers each task's stdout until the task ends, so nothing a corpus task prints to stdout is visible between `TASK start` and `TASK pass`. What *is* visible live is the progress channel — the one carrying the `FAIL [...]` markers — and `ci/run-tests.sh` ticks a heartbeat onto it:

```text
ci/run-tests.sh: LIVE checks/orchestrators/golden-tests: progress: 14/20 impl-run units done; 598/875 checks-only units done; 612/895 total units done, 8 running, 275 queued; 1204 passing rows; 18m elapsed
ci/run-tests.sh: LIVE checks/orchestrators/golden-tests: progress: no unit completed in the last 300s; in flight: 00_success/exec_99_bottles 00_success/exec_align_arm_collapse
```

(The `checks/orchestrators/<name>:` segment is the task name, present only under `ci/all.sh`. Run standalone, `run-tests.sh` prints the same lines to stderr without it.)

Read it as follows. With case narrowing active, the first fraction counts only
units carrying selected build-and-run implementation work; its denominator is
therefore the effective selected cohort, not the full corpus. The second
fraction counts the narrowed-out units performing only case-binary checks, and
the third tracks
all dispatched units. Without case narrowing there is one all-unit fraction.
`running` and `queued` are split because a unit can be waiting on a
`ci/schedule.sh` slot rather than executing — normal contention, not a stall.
The counts are result *rows* (one per impl per case, plus one per case-binary
tag), so they do not sum to `units done`. A **failure count is appended only
when it is non-zero** (`; 3
FAILING rows`): this stream is stdout under `ci/all.sh`, i.e. the text a reader
or an agent greps for failure markers, and a heartbeat that said `0 failed`
every interval would turn every such grep into a false positive on a green run.
A `fail` hit here is always real.

The second line appears on any tick where nothing finished, and it names the in-flight cases (at most six, then `...`): that is the `0.0s cpu` wedge diagnosis below, delivered live and already narrowed to the suspect units. The tick is on a timer rather than on unit completions precisely so a stalled run — which completes nothing — still reports; a completion-triggered line would fall silent in exactly the state worth seeing.

The interval defaults to 300s; `KIO_DEBUG_PROGRESS_INTERVAL=<seconds>` overrides it and `0` disables it. It is a debug-only surface (`AGENTS.md` § Universal rules) and stays out of `specs/` and `docs/`. The heartbeat is skipped where it has nothing to add or nothing to read: single-unit runs and `--show-output` / `--update-expected`, which stream per unit already. Serial units still use scheduled workers, so `--jobs=1` changes concurrency without bypassing admission.

**Detecting completion: anchor the `DONE` match.** `ci/all.sh`'s startup banner prints the sentinel strings themselves (`final line will be 'ci/all.sh: DONE pass', …`), so a substring `grep 'ci/all.sh: DONE'` matches that banner on the *first* line and reports completion before any work has run — a false positive that especially bites when polling a backgrounded run. Match the real verdict anchored at line start, `grep -E '^ci/all\.sh: DONE (pass|fail|aborted)'`, or read the actual last line. A `DONE fail` whose failing task shows `0.0s cpu` over a large wall time is a hang (a wedged child), not a test that did work and failed — triage the stuck process, do not just rerun.

Agents and other non-interactive drivers running a broad `ci/all.sh` gate should normally retain per-task logs for the duration of the report, then delete them after extracting the summary and any failure details:

```sh
sh ci/all.sh SAMPLE_IMPL --keep-logs
```

`ci/all.sh` prints the auto-generated log path at startup and repeats it at the end as `logs kept at <dir>/logs/`. Do not leave retained `ci/all.sh` log directories behind after pass/fail/abort triage; remove the printed parent directory once the summary and relevant task logs have been reported. Without `--keep-logs`, `ci/all.sh` deletes its temporary logs by default on pass, fail, and catchable abort.

## Scoping a test pass

Each layer below scopes by a different dimension. Pick the narrowest knob that still covers what your change could break.

- **Individual gating script** — `sh ci/checks/<bucket>/<name>.sh`. Use when only one bucket is affected (e.g., a markdown-only change → `sh ci/checks/repo-lint/markdown-lint.sh`).
- **Cargo-level iteration inside `kio-rs/`** — while developing Rust code, prefer the narrowest Cargo command through the repository wrapper (`sh ../ci/cargo.sh test <name>`, `sh ../ci/cargo.sh test --lib <module_filter>`, `sh ../ci/cargo.sh clippy --all-targets -- -D warnings`, or the relevant single-feature command). Save `sh ../ci/checks/hygiene/kio-rs.sh` for integration points or changes that could affect the feature split; it intentionally recompiles/checks several feature configurations.
- **Orchestrator-level scoping** — `sh ci/checks/orchestrators/golden-tests.sh` (and siblings: `emissions-tests.sh`, `generative-tests.sh`, `poc-tests.sh`, `castle-tests.sh`) accept `--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<impl>[,<impl>...]`. See [`TESTING.md`](../../TESTING.md) § Local iteration for the full interaction table.
  - `--impls=kio@js` — restrict to the JS impl. Skips Rust-backend work entirely.
  - `--impls=kio@rust` — restrict to the Rust impl.
  - `--impls=SAMPLE_IMPL` — for each case, pick one applicable impl at random per run. Roughly halves wall time at the cost of full backend-divergence coverage in a single run.
- **Case-level filtering** — `sh ci/run-tests.sh` takes positional filter arguments matched against case names (relative path under `--cases-dir`). Useful when iterating on a specific golden:

  From the repository root, set `KIO_BIN`, `KIO_PRIME_BIN`, and `KIO_RUNNER` to existing executable paths for the full compiler, reduced Kio' compiler, and JS runner. The direct harness requires `--cache-base`; this example uses the shared golden runner-artifact cache described in [`local-tools.md`](local-tools.md) § Compiler cache:

  ```sh
  CACHE_BASE="${XDG_CACHE_HOME:-$HOME/.cache}/kio/goldens"
  mkdir -p "$CACHE_BASE"
  sh ci/run-tests.sh --cases-dir=test-data/goldens \
    --cache-base="$CACHE_BASE" \
    "--impl-def=name=kio@js,kio=$KIO_BIN,runner=$KIO_RUNNER,target=js,prime-kio=$KIO_PRIME_BIN" \
    '^00_success/exec_hello_world$'
  ```

  `prime-kio=` is optional unless an enabled check declares `# REQUIRES: prime-kio`; `kio-prime-roundtrip.sh` does. `--exclude=<regex>` is also available (POSIX ERE, repeatable). Most invocations should go through the orchestrators above, which build and snapshot the reduced compiler and assemble the `--impl-def=` lines and `--check=` rows for you; reach for `ci/run-tests.sh` directly only when scoping to one or two cases.
- **Bucket-level filtering via the orchestrators** — pass positional filters after `--`. Example: `sh ci/checks/orchestrators/golden-tests.sh --impls=kio@rust -- 00_success` runs only the success bucket on the Rust impl.
- **Run each new or behaviorally edited golden on all applicable impls, with the case axis kept exact** — use `--impls=FULL_IMPL_MATRIX --all-cases` with no conflicting global/root/leaf case policy or raw harness sampling override unless a phase/CLI subject is inherently single-implementation; generated host-backend inspection is an emission subject, not an exception. Classify behavior versus mechanical migration using § Local gate coverage, not changed-file counts. Multiple exact selectors may be batched; do not widen them to a bucket or treat an effectively corpus-wide exact list as focused. Codegen changes or divergence can require more exact cases against affected backends; state the uncovered risk and honor the aggregate-scope checkpoint. Example: `sh ci/checks/orchestrators/golden-tests.sh --impls=FULL_IMPL_MATRIX --all-cases -- '^00_success/my_case$'`.
- **Run each new or changed emission case exactly** — use `sh ci/checks/orchestrators/emissions-tests.sh --impls=FULL_IMPL_MATRIX -- '^<backend>/<case>$'`. The backend-first layout makes the case single-backend by construction; exact filtering keeps the case axis narrow.
- **Reach for `--prime-only`** when the change touches Kio'-grammar surface (parser, lexer, the verifier). Only IS_KIO_PRIME-marked cases run.
- **Scope the generative batch** — `sh ci/all.sh SAMPLE_IMPL --gen-count=<N>` overrides the generative-tests orchestrator's default generated-program count. The flag is forwarded as `--count=<N>`, so `sh ci/checks/orchestrators/generative-tests.sh --count=<N>` is the direct equivalent. `--gen-seed=<N>` forwards a deterministic seed as `--seed=<N>` for reproducing or pinning generated cases.
  When `checks/orchestrators/generative-tests` fails under `ci/all.sh`, the failed-task diagnostics repeat the `kio-gen: seed = <N>`, `kio-gen: count = <N>`, and `kio-gen: prime-only = <bool>` lines. Reproduce the batch with `sh ci/all.sh SAMPLE_IMPL --gen-seed=<N> --gen-count=<N>` or directly with `sh ci/checks/orchestrators/generative-tests.sh --seed=<N> --count=<N>`. For a single case, use the case name and implementation from the `FAIL [<impl>] <case>` line with the direct orchestrator command, for example `sh ci/checks/orchestrators/generative-tests.sh --impls=<impl> --seed=<N> --count=<N> -- <case>`.
- **Choose top-level implementation coverage explicitly** — `ci/all.sh` accepts exactly one implementation-coverage choice before or after its options:
  - `sh ci/all.sh SAMPLE_IMPL` — forwards `--impls=SAMPLE_IMPL` to every orchestrator that supports it (`golden-tests`, `emissions-tests`, `generative-tests`, `poc-tests`, `castle-tests`). Each case runs on one of its applicable impls, picked at random per run; an emission case has only its bucket's backend target.
  - `sh ci/all.sh FULL_IMPL_MATRIX` — runs the full implementation matrix.
  - `sh ci/all.sh kio@js` or `sh ci/all.sh kio@js kio@rust` — forwards the selected impl list as `--impls=<list>` to the orchestrators that support it. A comma-separated list is also accepted.
- **Scope case coverage — a different axis from impl coverage** — how many *cases* of each corpus build and run, independent of how many *impls* each runs on. The two compose. `TESTING.md` § Case coverage is the full reference; the operational points:
  - Case narrowing scopes the **build+run tier only**. A case narrowed out by sampling, a numeric cap, or a named set still runs every `# ROUTING: case-binary` check (`fmt-canonical`, `dep-canonical`, `prime-marker`, `warm-recheck-stable`), so formatting drift, materialized-dependency-tree drift, and the `IS_KIO_PRIME` biconditional stay gated on the whole filtered corpus on every pass. Build/run coverage of cases is traded away, not the implementation selector. Preserve exact runtime coverage for affected behavior on every applicable backend; choose broader dynamic coverage using the risk-based rule in § Local gate coverage, not merely because codegen changed.
  - Defaults with no flag: regular goldens/direct Prime **all**, dynamic Prime **sampled** (50), emissions **all**, POC **all**, castles **sampled** (10), contrib **sampled** (10). This is the everyday local gate.
  - `sh ci/all.sh SAMPLE_IMPL --sample-cases` — sample every corpus. The fast gate; the normal GitHub command additionally activates `dyn-load-prime` and selects 10 seeded cases from its success-only cohort. Appropriate locally for a broad sanity pass on a change whose blast radius is not the corpus itself.
  - `sh ci/all.sh SAMPLE_IMPL --all-cases` — every case of every corpus. The exhaustive pass; the only way to get every castle through the local gate.
  - `--case-coverage=<scope>:<all|sample|N|named-set>` — repeatable registered root/leaf override, winning over `--sample-cases` / `--all-cases`. Explicit leaf > explicit root > explicit global > default; the last setting within one scope wins. `sample` uses 100 for regular/direct Prime and 50 for dynamic Prime; explicit numeric caps remain unchanged. An implicit leaf default never activates an inactive verifier. `N` caps cases **per top-level bucket**. Use `--case-coverage=castles:all` to run the full castle corpus without paying for anything else.
  - `--case-seed=<S>` — pin the draw. One seed covers every sampling corpus, so a single seed replays the whole run's selection; it is inert for a corpus running every case. Each sampling orchestrator prints its effective seed, per-bucket tallies, and selected case names, so a sampled failure always reproduces: rerun with the printed seed, or narrow to the failing case name directly.
  - **A `KNOWN_FAILING` case is never narrowed out.** Its gate — a bug reproducer that starts passing must fail, so the marker gets cleaned up — lives in the impl tier, and its case-binary checks are skipped by contract, so a narrowed-out one would run nothing at all. Sampling, numeric caps, and named sets pin it into the impl tier.
  - A cap of `0` is rejected at every layer: a green run that built and ran nothing is the opt-out `AGENTS.md` § Universal rules forbids.
- **`--no-live`** — suppress the live progress stream (task starts, completions, per-failure diagnostics, and the § Progress heartbeat). Failures still appear in the end-of-run report. Useful when capturing output to a file that a human will read only after the fact; leave it off when watching a run.
- **Omit scheduler overrides in ordinary use** — `ci/all.sh` obtains shared work capacity from the scheduler's `std::thread::available_parallelism` query and uses best-effort shared adaptive compiler feedback on Linux, macOS, and Windows. Use `--jobs=<N>` or `--compiler-jobs=<N>` only to reproduce scheduler behavior, deliberately constrain a run, or apply a measured machine-specific capacity; a numeric compiler value is a fixed hard cap. Top-level Cargo invocations use compiler admission by default through `ci/cargo.sh`; Cargo reached from a scheduled worker retains that worker's work lease. Set `KIO_CI_SERIALIZE_CARGO=1` only when whole-Cargo capacity-one serialization is desired.

**When scoping is *not* appropriate:**

- Changes to `ci/run-tests.sh`, the orchestrators, or the harness infra under `ci/infra/` — a single scoped case run is not enough. Use the broad sampled gate, and add focused `--impls=FULL_IMPL_MATRIX` or explicit-backend runs when the change touches matrix expansion, backend dispatch, or backend-divergence behavior.
- Spec changes that affect multiple buckets (e.g., a new exit-code category, a grammar change that ripples through goldens + generative + prime-marker checks).

### Flags vs. environment

User-facing CI behavior should be explicit command-line state: implementation coverage uses `SAMPLE_IMPL` / `FULL_IMPL_MATRIX` / explicit impl names, case coverage uses `--sample-cases` / `--all-cases` / repeatable `--case-coverage=<scope>:<policy>` root-or-leaf overrides / `--case-seed=`, generated-case count and seed use `--gen-count=` / `--count=` and `--gen-seed=` / `--seed=`, scheduler overrides use `--jobs=<N>` and `--compiler-jobs=<N>`, and log retention uses `--keep-logs` or `--keep-logs=<DIR>`.

**Naming rule for the two coverage axes.** A flag that cuts *implementations* names `impl`; a flag that cuts *cases* names `case`. Neither is ever spelled as a bare `--sample` or `--all`, because at a glance the two are indistinguishable and they mean very different things — `--impls=SAMPLE_IMPL` runs every case on one backend, `--sample-cases` runs some cases on every backend. Keep that discipline when adding a coverage knob.

`ci/all.sh` deletes auto-created logs after pass/fail/abort unless retention was requested, but failing runs repeat failed-task diagnostics at the end so the actionable failure is not buried in earlier buffered output.

Environment variables are for execution context passed to child scripts (`KIO_BIN`, `KIO_PRIME_BIN`, `KIO_RUNNER`, `KIO_TARGET`, `KIO_TEST_UPDATE`, `KIO_PRIME_CHECK_BIN`), external tool behavior (`RUSTC_WRAPPER`, `CARGO_BUILD_JOBS`, GitHub credentials), or the explicit scheduler execution-context opt-in `KIO_CI_SERIALIZE_CARGO=1`. `KIO_PRIME_BIN` is derived from the explicit per-implementation `prime-kio=` field rather than ambient shell state. Debug, profiling, and timing-only switches are the narrow Kio-owned exception: they must use `KIO_DEBUG_...` names (or a `kio debug ...` / `--debug-...` surface) and stay documented in internal guidance such as this file or [`local-performance.md`](local-performance.md), not public `specs/` or `docs/`. `KIO_DEBUG_PROGRESS_INTERVAL` (§ Progress heartbeat) is one. Avoid adding new env-var switches for ordinary test selection; hidden shell state makes CI runs hard to reproduce.

The internal `KIO_DEBUG_SAMPLE_IMPL_SEED` probe is the narrow exception used
to hold randomized implementation assignments constant across performance
comparisons. It does not change the ordinary command or coverage contract; see
[`local-performance.md`](local-performance.md) § Machine shape and contention
for its validation, mapping, and evidence rules.

### Changing the CI command-line surface updates the instructions with it

`ci/all.sh`, `ci/run-tests.sh`, and the corpus orchestrators under `ci/checks/orchestrators/` are the commands agents are *instructed* to run, so their command-line surface is load-bearing documentation, not an implementation detail. An agent that reads a stale flag here runs the wrong gate and reports a coverage claim it did not earn.

So a change to any of those scripts' flags, defaults, or coverage semantics lands **in the same commit** as the corresponding updates to:

- [`AGENTS.md`](../../AGENTS.md) § Universal rules — the local-CI rule, which names the baseline gate every conversation is expected to know before its first action.
- [`TESTING.md`](../../TESTING.md) § Local iteration — the selector-syntax reference the universal rule points at, including § Case coverage.
- **This file** — § Local gate coverage and § Scoping a test pass, which tell an agent which knob scopes by which dimension.
- The affected corpus README, including [`test-data/emissions/README.md`](../../test-data/emissions/README.md) for backend-first sampling and marker semantics.
- [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml), when the change alters what the hosted gate should pass.

Renaming or removing a flag without sweeping these is the failure this rule exists to prevent. **`git grep` the old flag across the whole tree**, not a remembered subset of it: `test-data/*/README.md` document the orchestrator commands too, and a sweep that skips them leaves copy-pasteable commands that now exit 2. [`audit-local-running-guidance`](../skills/audit-local-running-guidance/SKILL.md) § 6 checks the pairing mechanically — every long flag in `ci/all.sh`'s usage must appear in `TESTING.md` or this file — but a mechanical check cannot tell you a *prose claim* went stale, so read the surrounding sentences too.

## Kio-semantic caches

The package-check, typed-module, enriched-IR, emit/artifact, equiv, and Kiodoc snippet caches are keyed by inputs that include the build-time compiler cache identity. That identity folds in the package version, enabled feature set, Cargo manifests, and Rust source files, so a workspace-local compiler edit followed by a rebuild retires stale semantic entries automatically.

Long-lived package cache roots also carry access metadata for `kio cache gc`. A successful cache-using command may run a throttled GC pass for roots it touched; explicit `kio cache gc` runs the sweep immediately. The GC policy is not a CI isolation mechanism.

The corpus harness runs `<binary> cache clear` once per `(case, binary)`
sequence before that pair's impls run, so its package-local entries start empty
and the within-unit warm hits stay coherent. Marked custom cases instead clear
each private execution copy after source checks; `--keep-cache` skips either
clear. Ordinary harness-owned units
additionally share one content-addressed typed-module root scoped to that
harness invocation, so byte-identical module inputs copied into distinct
package workdirs can reuse typechecking. A custom `run.sh` instead receives a
root under its per-case, per-implementation scratch directory by default, so
arbitrary case logic cannot alter another case's typed entries. The regular
golden orchestrator gives one separately reviewed custom cohort its own
run-scoped root: a harness-owned manifest binds every allowed relative
`run.sh` path to its exact Git object ID, and `run-tests.sh` snapshots and
reauthenticates those scripts before execution. The internal cohort option is
not derived from a case name, marker, or case-provided environment. Unlisted
scripts remain isolated and stale manifest entries stop before dispatch.
Cache-specific checks shadow their inherited root when their subject requires
a genuinely cold first command. Every harness-owned typed root is removed with
that run.
The `--no-cache` flag remains available on `kio` and `kio-prime` for ad-hoc
cold runs but is not wired through CI.

The separate test-runner **artifact** cache (the Rust runner's `rlib`/`bin` and the Go / Haskell `bin` caches) is content-addressed by toolchain + source bytes and stays on regardless. Unlike the build caches above, the orchestrators default it to a machine-stable shared root so sibling worktrees reuse compiled binaries; see [`local-tools.md`](local-tools.md) § Compiler cache for the shared location, the size-LRU bound, and the worktree-local override.

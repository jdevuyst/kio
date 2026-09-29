# Kio CI scheduler

This private Rust crate is the one scheduling authority used by Kio's local
and GitHub CI entry points. It schedules work; it does not discover checks or
decide which corpus cases and implementations run.

Ordinary shell callers use `ci/schedule.sh`. That file is a compatibility
facade: it captures whether the caller's standard input is open, discovers the
Git common directory, resolves the immutable scheduler binary, maps its stable
shell syntax onto the native CLI, and explicitly registers any readiness hook.
On Windows the facade and bootstrap run in the repository's POSIX `sh`
environment and require `cygpath` to bridge shell and native paths; the
resolved scheduler is still a native Windows executable. Resource policy,
queue state, locking, held-resource validation, and
process-tree supervision live here. Native test runners link this crate so
shell-launched and runner-launched compilers use the same compiler policy and
disk protocol.

## Resources and acquisition order

The scheduler owns three independent resources:

- `work` limits corpus and broad-gate work units. A barrier still consumes one
  work slot, but blocks fresh normal admission until every earlier active unit
  has drained.
- `cargo` is an optional capacity-one serialization resource for top-level
  repository Cargo invocations across sibling worktrees.
- `compiler` covers top-level Cargo, compiler-producing Kio commands, and
  native host compilers. An explicit capacity is a hard ceiling; omission uses
  the conservative policy described below.

The only valid acquisition order is `work -> cargo -> compiler`. Omitting a
resource is valid, so `work -> compiler`, `cargo -> compiler`, and `compiler`
are ordinary paths. `KIO_CI_SCHEDULE_HELD` records a unique canonical subset
in that order. Re-entering a held resource reuses it; trying to acquire an
earlier resource after a later one is an error. A nested barrier cannot
strengthen an already-held normal work lease and therefore reuses that lease.

`KIO_CI_SERIALIZE_CARGO=1` opts a top-level `ci/cargo.sh` invocation into the
Git-common `cargo` resource. It replaces the old arbitrary-path external Cargo
lock: it intentionally coordinates this repository and its sibling worktrees,
not unrelated repositories. Cargo invoked inside emitted projects or fixture
crates remains direct so it cannot recursively acquire repository resources.

## State and crash consistency

Each resource uses the same layout below `KIO_CI_SCHEDULE_DIR`:

```text
<resource>/
  state.lock
  claims/
    <immutable-id>.lease
    <immutable-id>.pending | <immutable-id>.active
```

The immutable identifier encodes protocol version, live-queue ticket, claim
kind, and requested capacity. The locked lease is deliberately zero length;
state is represented by separate markers. This avoids reading, truncating, or
renaming a file while another process has it locked, operations whose sharing
semantics differ on Windows. Activation creates `active` before removing
`pending`; if both appear after an interruption, `active` wins. Scanners hold
`state.lock`, prove liveness only with a non-blocking lease-lock attempt, close
an unlocked lease before removing its files, and remove orphan markers. An
unknown live protocol record fails admission closed. Queue tickets derive from
live claims, so there is no truncate-and-rewrite counter that can poison later
admission after a crash; an empty queue may safely restart at ticket one.

All active clients use the same protocol and Git-common state root. Separate
concurrent state layouts would permit oversubscription.

## Process-tree lifetime

Admission belongs to the complete launched process tree, not just its leader.

On Unix, an admitted child inherits the locked lease's open file description.
`KIO_CI_SCHEDULE_LEASE_FDS` is a strict inventory of those private descriptors;
values must be unique canonical integers above the standard descriptors. A
short-lived leader may exit without freeing capacity while any descendant
still owns a copy. The Unix invocation waits for and returns the leader's exit
status; it does not wait for every descendant, but later admission remains
blocked until the last inherited lease descriptor closes. Catchable supervisor
signals are forwarded to the child; even an uncatchable supervisor death cannot
make a surviving descendant's lease appear stale. A genuinely closed caller
stdin remains closed for the Unix child.

On Windows, the scheduler creates the child suspended, creates an unnamed Job
Object with kill-on-close, assigns the child before any user code runs, resumes
its primary thread, preserves the leader's exit status, and then waits for the
Job's authoritative active-process count to reach zero. Assignment or resume
failure kills and reaps the suspended child. If the supervisor dies, closing
its last Job handle terminates the tree before the permit can be reused.
Windows 8 or newer supports compatible nested Jobs; a host Job with
incompatible restrictions makes assignment fail closed rather than allowing
an unsupervised launch. When the caller's stdin was closed, the Windows adapter
uses `NUL` so the child observes EOF without receiving an invalid native
handle. The adapter owns the admitted command's entire Windows creation-flags
field and sets `CREATE_SUSPENDED`; callers of this internal API must not depend
on preserving preconfigured creation flags.

The Kio Job permits explicit breakaway because the readiness hook described
below must not trap a daemon in an enclosing Kio resource. Ordinary admitted
commands never request breakaway, but a command that deliberately creates a
child with `CREATE_BREAKAWAY_FROM_JOB` can escape. This scheduler is cooperative
resource control, not a security sandbox. An outer host Job may still reject
breakaway; escaping Kio's own nested Jobs is the invariant needed for correct
draining.

## Generic readiness hook

The optional post-admission hook is a tool-neutral command vector: one program
plus zero or more prefix arguments. The scheduler appends
`--compiler-readiness -- <target> <target-args...>`, runs it after a compiler
queue wait and before target lease inheritance or Job creation, and refuses to
launch the target if the hook fails. Library callers receive the same vector
through the strictly counted private environment variables
`KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM`,
`KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT`, and
`KIO_CI_SCHEDULE_READINESS_HOOK_ARG_<N>`.

That probe belongs to a newly acquired compiler lease. A nested compiler
command carrying the canonical inherited `compiler` marker reuses both that
lease and its already-established readiness instead of spawning another probe;
an independent later acquisition probes again. No result is persisted between
leases, and malformed inherited markers fail closed rather than granting reuse.

The hook cannot retain Kio resources. On Unix its child closes every descriptor
in the validated lease inventory before `exec` and receives neither the
inventory nor `KIO_CI_SCHEDULE_HELD`. On Windows it alone is launched with
breakaway semantics and waits only for the hook leader, allowing a daemon it
starts to outlive the hook without pinning a Kio Job.

The Rust scheduler does not recognize sccache or match executable basenames.
`ci/infra/sccache.sh` is the explicit tooling adapter: it owns wrapper
recognition, sets the daemon's idle-lifetime policy, and registers this hook
only when the current command can use that wrapper. The semantic Kio compiler
proxy supplies the generic private `KIO_CI_SCHEDULE_READINESS=SKIP` proof
because it typechecks and emits source without invoking the test runner's
native compiler wrapper. The shell facade consumes and removes that marker
before target launch, so it cannot suppress readiness for a nested command.
Adding another daemon-aware tool requires an equally explicit adapter and
documentation; no tool-specific behavior belongs in the scheduler core.
For its small declared tool vocabulary, the adapter accepts either path
separator, strips `.exe`, and normalizes ASCII case so native Windows command
paths do not silently miss the hook.

## Omitted compiler capacity

Omitted capacity uses best-effort aggregate feedback. One bounded transient
`compiler/feedback-v1` record shares its target and sample cadence under the
existing claim-store lock. Only the valid FIFO head advances it. Clients
honor the minimum live adaptive CPU ceiling, shared target, and explicit fixed
capacity; fixed-only activity bypasses feedback. Idle queues reset learning.

The target starts at one. Comparable native CPU/memory samples, queued demand,
and spare capacity permit paced additive growth; low available memory or an
available pressure signal reduces subsequent admissions. Missing, stale,
malformed, or unwritable state and unavailable comparable observations fall
back to one. Samples, including unavailable results, are throttled; idle
commands do not invoke native probes. The CPU ceiling comes from
`std::thread::available_parallelism`. Whole-host utilization cannot establish
spare capacity inside a smaller reported CPU domain, so a scope mismatch uses
the fallback rather than extrapolating.

Linux reads bounded aggregate CPU/memory counters and exposed local cgroup
memory limits, with optional memory PSI. macOS uses native CPU counters and
free pages, excluding estimated inactive-page reclamation. Windows uses native
CPU, physical-memory and commit-headroom counters; its group-limited CPU API
falls back above one processor group. These observations are not complete
accounting of hidden resource domains or future allocation bursts. Even one
arbitrary compiler command can exhaust memory; this policy makes no universal
OOM guarantee.

`--compiler-jobs=<N>` remains an explicit fixed override, never inferred from
a command name. Admission stays prospective and lease-based: pressure neither
suspends nor terminates already-admitted commands.

## Compiler-admission trace

Set `KIO_DEBUG_CI_SCHEDULER_TRACE=<run-token>` to record the compiler
admission decisions made by independent claims. The token is a portable path
component: 1–64 ASCII letters, digits, `-`, or `_`, excluding reserved
portable filenames. Each acquisition creates a unique
`<KIO_CI_SCHEDULE_DIR>/debug/compiler-admission/<run-token>/<pid>-<sequence>.jsonl`
file with `create_new`; repeated queue tickets therefore cannot overwrite an
earlier acquisition. Disabled scheduling and inherited compiler-lease reuse do
not create a new file.

Every line uses schema `kio-ci-compiler-admission-v3`. It carries one wall-clock
anchor plus monotonic elapsed time, process and acquisition identity, queue and
capacity state, the fixed/adaptive mode, the decision, and its typed blockers.
An adaptive claim stopped at its effective bound records
`adaptive-cap`; a smaller explicit request records `fixed-cap`. Evaluations
that do not consult feedback carry a null `adaptive_capacity`; non-head
adaptive evaluations also carry a null `effective_limit`. Repeated
waits are emitted only when their semantic queue state or blocker changes.

Trace files persist until explicitly removed; the scheduler does not rotate or
garbage-collect them. With the default state discovery, find a token's files at
`<git-common-dir>/kio-ci-schedule/debug/compiler-admission/<run-token>/`.
After every process in that traced run has exited, remove that token directory
when its evidence is no longer needed. A trace setup or write failure fails the
affected acquisition as a compiler-admission error rather than silently leaving
an incomplete diagnostic record.

The target is attributed only by the hex-encoded bytes of its generic program
basename. The scheduler never classifies that basename or recognizes a
compiler, wrapper, backend, or cache tool by name. The trace records admission
decisions, not a synthetic release event: process-tree lease lifetime remains
authoritative. Trace directory creation starts only after queue-ticket
publication, and all trace setup and writes occur outside `state.lock`.
Enabling the probe does not change admission policy.

## Bootstrap, self-test, and platform evidence

The scheduler cannot schedule its own first build. `bootstrap.sh` therefore
has one narrow exception: it hashes scheduler sources, lockfile, applicable
Cargo configuration, selected Rust host/toolchain, Cargo identity, and
output-affecting flags; builds exactly this binary into a keyed private target;
and publishes an immutable executable with a hard-link race. A hot lookup
validates `--build-id` and performs no Cargo work. Debug info and incremental
state are disabled for this small bootstrap artifact so compiling the scheduler
does not become a recurring gate bottleneck.

Direct environment wrapper settings receive an additional resolved-path and
executable-content identity. Scheduler sources, Cargo configuration, direct
wrappers, and Cargo's resolved executable identity are rechecked after a cold
build before publication. The selected rustc is identified by its command and
complete `rustc -vV` report; that compiler and the bootstrap's other external
tool references must remain stable during one resolution. The bootstrap does
not parse Cargo's layered configuration language or recursively hash tools
named only inside Cargo config/build flags, nor a wrapper's interpreter or
other transitive dependencies. After replacing one in place without changing
its recorded identity, change the owning config/flag identity or remove the
Git-common scheduler cache before resolving the binary again.

`ci/all.sh` resolves the binary once, runs the native `self-test`, obtains
default work capacity through `available-parallelism`, and only then fans out.
The hot self-test uses isolated scheduler state and exercises native resource
locks and supervised process behavior without invoking Cargo or hashing
sources. The macOS and Windows portability jobs run that same self-test plus
the crate tests natively. The Windows-only
`scheduler_job_nests_inside_an_outer_job` test creates a real outer Job before
exercising the scheduler's inner Job. Cross-target checks remain useful compile
evidence, but cannot prove advisory-lock semantics, inherited Unix descriptors,
suspended Windows spawn/assignment, nested Jobs, breakaway, or complete Job
draining.

See `ai/topics/local-ci.md` for operator-facing scheduling behavior and
`ai/topics/local-tools.md` for the Cargo and compiler-cache entry points.

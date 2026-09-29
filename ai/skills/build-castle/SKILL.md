---
name: build-castle
description: "Build exactly one new castle under test-data/castles/ — a larger coherent Kio project adding corpus diversity: natural source, run.args-only, success-only; any discovered Kio bug is fixed with a focused regression golden"
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Skill, Agent
---

# Build a castle

A **castle** is a larger, coherent Kio program — an algorithm, a toy
game, a parser, a planner, a simulation, a puzzle solver, a
utility-shaped program — run end to end through the standard runner
path. Castles exist to exercise *realistic composition*: source that
looks like Kio someone would actually write for a small real project.
The corpus contract is
[`test-data/castles/README.md`](../../../test-data/castles/README.md).

This skill adds **exactly one** new castle per invocation. Each new
castle must increase corpus diversity: a novel domain, algorithm,
data shape, execution shape, host surface, or compiler-stress
combination relative to the castles already present. Surveying the
corpus before choosing is part of the job, not a formality.

When the user asks for many castles, treat the request as corpus
curation rather than repeated case generation — the batch discipline
lives in § Batches.

Castles should exercise ordinary surface Kio, not hand-written Kio'
glue. Avoid `import __intrinsics__;` and raw calls such as `__left__`,
`__right__`, `__either__`, `__pair__`, `__fst__`, and `__snd__` unless
the castle is explicitly about Kio' / intrinsic behavior. Prefer
`widen_sum!`, `match!`, labels/newtypes, tuple syntax, row access, and
`rec(loop)`. If a candidate builder reaches for raw intrinsics during
normal castle authoring, treat that as a guidance/brief problem:
tighten the public surface-level docs or the supervisor brief instead of
promoting the raw-intrinsic draft. For repeated same-typed sum branches,
prefer labels/newtypes over direct left/right injection.

Bulk castle work is candidate-driven. Before scaling a batch, run
candidate-builder mode (§ Candidate-builder mode) on a representative
set: at minimum one compact castle, one medium-or-large castle, and one
example from each architecture family the batch will lean on. Each
candidate builder owns a real castle attempt end to end: it reads the
allowed public/corpus docs, writes the program, runs `kio check` and
`kio build` from the candidate package `workdir`, runs the selected
backend's runner, and runs the castle harness where practical, then hands
back the candidate plus a documentation-gap report. The supervisor
reviews the candidate and report, updates
documentation where the builder exposed a real gap, and decides whether
the castle is retained, revised, or rejected. Do not claim the batch is
complete unless this gate ran and its result is reported. If the
environment cannot provide sub-agents, pause before bulk work and tell
the user that the candidate-builder gate cannot be run as designed.

The candidate gate is a stop-the-line gate, not a formality. If a
candidate finds unclear language/package knowledge, an underspecified
supervisor brief, a mismatch between the requested program shape and
the selected runner protocol, or any uncertainty about where protocol
facts are documented, **do not start bulk implementation**. Classify
and fix the finding first, then rerun or update the affected candidate.
A large batch must not proceed from "the examples probably work", from
unreviewed generated templates, or from a supervisor-generated pile of
castle directories.

The candidate gate tests whether documented Kio authoring and corpus
runner documentation are sufficient for a worker to build and verify a
castle. It is **not** a reason to move runner mechanics into public
language docs. The builder's allowed and forbidden reading sets are
stated once, in § Candidate-builder mode step 4; the supervisor may read
the forbidden sources, but a fact discovered there is not builder-usable
until it is routed to the correct durable home.

Measure size by the castle's own source as well as by integrated
dependencies, but report the owned-source tier separately. Owned source
excludes materialized dependency closures and repeated protocol boilerplate
such as copied `testapi` env declarations. Path dependencies on reusable
POC packages are encouraged when they make the project more realistic,
but shared POC modules do not by themselves make a castle "large"; they
count as integration breadth, not as a substitute for castle-owned
structure.

A castle is **not** a minimized regression golden, an adopter-grade POC
reference library, or a workaround for today's compiler; the bug
discipline is workflow step 10 below.

## Castle layout (the actual convention)

Each castle is a **direct child** of `test-data/castles/` — a flat
directory, no exit-code buckets, no nested case directories:

```text
test-data/castles/<name>/
  README.md
  workdir/
    <pkg>.pkg.kio        # package file, carrying the build { ... } block
    ...                  # the regular-module tree that provides main
  run.args
  expected.stdout
  expected.stderr.ignore
  expected.exit          # exactly: 0
  input.stdin            # present iff the package reads stdin
```

The package's regular module tree lives **directly under `workdir/`**
(e.g. `workdir/<pkg>.pkg.kio`, `workdir/maze/world.kio`). There is no
`src/` subdirectory and no `<pkg>.downstream.kio` file — those are not
Kio conventions. Match the direct-child package shape described by the
castle corpus contract.

The case runs through the same [`ci/run-tests.sh`](../../../ci/run-tests.sh)
harness as goldens and POCs, so the shared per-case marker menu and
per-case-check pipeline apply identically. See
[`test-data/README.md`](../../../test-data/README.md) § Golden test
case layout for the shared file contract.

## Workflow

This workflow is for the supervisor or for a one-off castle authoring
task where the agent is not being used as a public-docs-only candidate
builder. A spawned candidate builder follows § Candidate-builder mode
instead: it is testing whether the public docs and runner docs are
sufficient, so it must not use `ai/topics/` as Kio authoring guidance.
The supervisor owns this workflow's `AGENTS.md` / topic-file reading
before spawning the builder and turns any candidate-discovered gap into
durable docs, runner docs, or skill guidance.

Do these in order. Steps 1–3 are mandatory reading before you touch any
`.kio` file.

1. **Read [`AGENTS.md`](../../../AGENTS.md)** — its § Universal rules and
   § Trigger table override default behavior. The load-bearing rules for
   this skill: no partial implementations, bugs surface (never hidden),
   goldens demonstrate behavior (not workarounds), per-backend
   limitations are documented only when genuinely impossible (and
   mutual-cited), and surface forms must not survive into Kio'.
2. **Read [`test-data/castles/README.md`](../../../test-data/castles/README.md)** —
   the authoritative corpus contract: success-only, `run.args`-only
   execution, `expected.exit` exactly `0`, `input.stdin` fixture
   semantics, seed-as-fixture, and output readability.
3. **Read [`ai/topics/kio-authoring.md`](../../topics/kio-authoring.md)**
   before editing `.kio` files (per the trigger table). It routes to the
   public guides under [`docs/`](../../../docs/README.md) and carries the
   agent deltas (the lowering mental model, fn-signature shape, package
   skeleton, qualified-import default, `pub`-only-for-exports,
   `&`-for-products). Its § Improving this guide is the authoring-feedback
   protocol this skill defers to (see § Doc-update discipline below).
4. **Survey `test-data/poc/` up front.** Inspect the directory live —
   do **not** hardcode a module list; the corpus changes over time:

   ```sh
   ls test-data/poc/
   ls test-data/poc/*/workdir/*.kio
   ```

   Summarize which freestanding packages and modules are currently
   available. When a current POC package fits the castle naturally — a
   solver, helper, data structure, elaborator package, or other library
   surface — reuse it through the dependency mechanism rather than
   copying or reimplementing it. For the shared elaborator library
   (`match!`, `derive!`, the spine palette), depend on the `elab`
   package with a `<local>.dep.kio` at the castle's package root:

   ```kio
   // elab.dep.kio
   dependency elab;
   source { path "<rel>/elab.pkg.kio"; }
   ```

   then import the re-rooted modules using the declarations shown in the
   current elaborator guides, and add `elab/testapi;` to the
   castle's `bridge` for the role types the elaborators reach. The
   dependency `path` must be relative from the castle package root;
   never write an absolute path, a home-directory path, a temp path, a
   local `file://` URL, or a sibling-worktree path into any checked-in
   castle file. The castle must still be a coherent project with its own
   domain, fixtures, orchestration, and corpus novelty, not a thin
   wrapper whose only purpose is to call a POC library. Do not vendor elab module files into
   a castle; depend on `elab` instead. Surface use of the spine-solver
   and `match!` solver modules is described in the
   elaborator guides under
   [`docs/guides/`](../../../docs/README.md) (e.g.
   `elaborators.md`, `using-libraries.md`,
   `sums.md`) — read the live `docs/` catalog rather than
   trusting a fixed filename here. For other POC packages, do not require
   a hand-written `docs/poc/<name>.md` case study. Their public package
   surface is the rendered Kiodoc from `///` comments; run `kio doc build
   --md` in the POC package workdir and use the generated
   `out/docs-md/` pages as the builder-facing API reference.
   When you add the castle's own `<local>.dep.kio` (e.g. depending on
   `elab` or another POC package), run `kio dep fetch` in the castle
   `workdir` and **commit** the materialized `<local>/…` tree it writes:
   a consumer ships its dependency's materialized closure, so the castle
   source includes the committed re-rooted tree (this is what lets a fresh
   checkout build the castle with no fetch step). The repo-lint
   `dep-materialization` gate asserts that tree is committed and canonical;
   regenerate it any time with `ci/checks/repo-lint/dep-materialization.sh
   --write`. Nested dependencies stay materialized-only: a depended-on POC
   that itself declares dependencies already ships its own committed
   closure, so fetching it is a single non-recursive step.
5. **Inspect existing castles.** List `test-data/castles/`, summarize
   the live corpus by protocol, stdin/no-stdin shape, owned source size,
   and visible architecture family, then read representative
   `README.md` files from the nearest families. For a small corpus, read
   every castle's README; for a large corpus, read enough exemplars to
   make the novelty claim concrete without turning survey into the main
   task.
6. **Inspect nearby goldens** enough to avoid duplicating an existing
   subject. A castle's value is composition the minimized goldens and
   generated Kio' programs miss; don't re-skin a golden as a castle.
7. **Choose one candidate target.** Use the diversity lens (§ Diversity
   lens below) to make the novelty concrete: name the domain, data
   shape, execution shape, host surface, and the compiler area it
   stresses, and how that combination differs from what's already there.
   Write a short candidate brief with the runner protocol, file-layout
   contract, dependency docs to read, and verification commands.
8. **Run candidate-builder mode** (§ Candidate-builder mode). The
   builder, not the supervisor, creates the candidate castle and runs
   the relevant checks. The candidate must include:
   - `README.md` — useful to a reader who has not read the source: what
     the program models or computes, the shape of `input.stdin` and
     where any fixture seed appears, what stdout means, and a final
     "What this adds to the corpus" paragraph naming the diversity.
   - `workdir/<pkg>.pkg.kio` plus the regular-module tree that provides
     `main`. Write **natural** Kio — qualified imports by default, `pub`
     only for intentional exports, `&` for products. Declare POC package
     dependencies per step 4 when reusing POC code.
   - `input.stdin` **iff** the package reads stdin (declares
     `read_ascii_line()`). A castle that declares `read_ascii_line()`
     *must* ship `input.stdin`; one that reads no input omits it.
   - `run.args` — the **only** execution file. Empty selects the
     standard build-then-run path; a non-empty file supplies plain
     whitespace-separated runner tokens (e.g. a `--protocol <tier>`
     selector). No `run.sh`, ever.
   - `expected.stdout`, `expected.stderr.ignore` (or another single
     stderr policy file per the shared contract), and `expected.exit`
     containing exactly `0`.
9. **Review the candidate.** Read the builder's source, fixture, stdout,
   command log, and documentation-gap report. Decide retain, revise, or
   reject. If the candidate needs non-trivial source changes, send it
   back to the builder or spawn a replacement candidate rather than
   silently rewriting the program as the supervisor. If retaining it,
   rerun the castle harness:

   ```sh
   sh ci/checks/orchestrators/castle-tests.sh -- <name>
   ```

   (Before any cargo-backed build, set the repo's configured compiler
   cache — see [`ai/topics/local-tools.md`](../../topics/local-tools.md)
   § Compiler cache — rather than routing into a throwaway cache.)
10. **If the castle exposes a Kio bug:** keep the natural castle source,
    minimize the bug into a focused regression golden under
    `test-data/goldens/` (a dedicated case that exercises the bug's
    shape *directly*), fix the bug in the same change, and verify both
    the golden and the castle (AGENTS.md § Universal rules — "Bugs
    surface; never hide them"). Do **not** document buggy behavior as
    intended; do **not** land a per-backend opt-out instead of fixing the
    backend. A per-backend shortcoming is fixed unless the host language
    genuinely cannot express it (then mutual-cite per AGENTS.md).
11. **Update durable guidance** when building the castle forced you to
    reverse-engineer Kio behavior and you found a reusable authoring
    insight. Follow
    [`ai/topics/kio-authoring.md`](../../topics/kio-authoring.md)
    § Improving this guide — it owns the decide/verify/route/keep-context-free
    loop and the blast-radius tier (see § Doc-update discipline below).
    [`audit-kio-guide`](../audit-kio-guide/SKILL.md) backstops the
    pointer discipline. Leave newly discovered Kio authoring knowledge
    in its durable home, never only in chat, scratchpad, or the castle
    source. Keep user-facing docs user-facing: do not add castle/corpus
    workflow, `test-data/castles/` instructions, or agent-only adoption
    paths to `docs/`; put castle procedure in this skill or
    `test-data/castles/README.md`, and put general language/package
    mechanics in the appropriate public guide.
12. **Leave a concise final note** naming: the new castle, the diversity
    it adds, any guidance files updated, any bug fixed plus its
    regression golden, the builder-owned checks, the supervisor review
    decision, and any rejected candidate count.

## Forbidden

Do not, as part of building one castle:

- add **multiple** castles in one invocation, unless the user
  explicitly asks for more than one;
- create **any execution file other than `run.args`** — no `run.sh`;
- land a castle whose `expected.exit` is **not exactly `0`** (a
  non-zero castle is a goldens-bucket regression case or a bug to fix,
  not a castle);
- **reshape castle source around compiler limitations** — adding
  explicit type annotations to dodge an inference gap, splitting a
  polymorphic newtype into monomorphic ones, monomorphising to skip a
  codegen path. Fix the emitter; let the source read naturally;
- use **arbitrary host I/O** — a castle's host surface is the canonical
  runner functions only (`read_ascii_line()`, the `string_*` helpers,
  `loop`, printing, the arithmetic/comparison families, `array_*`);
- **add runner canonical functions** as part of building one castle —
  if a castle truly needs a host capability the runner lacks, that is
  its own approved change, not a side effect of adding a castle;
- add **case-specific host parsing helpers** such as `parse_int`,
  `split_whitespace`, `read_command`, or `parse_grid`. Parse fixtures in
  Kio using the canonical string-inspection helpers (`string_len`,
  `string_slice`, `string_code_at`, `string_to_int`, `string_eq`,
  `string_concat`) plus Kio code;
- check in **absolute or checkout-local paths** anywhere in the castle
  case. `.dep.kio` `source { path ... }` entries are relative from the
  package root; READMEs, source, fixtures, expected output, and runner
  args must not mention temp directories, home directories, sibling
  worktrees, or local `file://` URLs;
- add a runner-provided **`seed()` host fn**. Randomness is fixture
  data: read the seed from `input.stdin`, document its line in the
  README, and implement any PRNG in Kio;
- **rely on the runner to echo fixture input.** The runner echoes
  nothing; make the Kio program print the transcript or summary it
  needs, so `expected.stdout` is understandable on its own;
- treat a castle as a **replacement for a minimized regression golden**
  — a discovered bug gets its own focused golden;
- leave a **discovered Kio bug unfixed** as part of a landed castle;
- leave newly discovered Kio authoring knowledge **only in chat,
  scratchpad, or the castle source**;
- synthesize a batch of castle programs as the supervisor. Scripts may
  help a builder with mechanical work inside a single candidate, or help
  the supervisor track assignments and accepted candidates, but they
  must not generate the batch's castle source in place of
  builder-owned attempts;
- **land a new or large `docs/guides/` page, or any `specs/` edit,
  autonomously.** Surface those as a proposal in your final note for the
  user to approve. Small `ai/topics/` sharpenings and small
  `docs/guides/` clarifications may land directly (see § Doc-update
  discipline);
- let a **candidate builder** resolve an unclear language rule from
  outside its allowed reading set (§ Candidate-builder mode step 4). The
  builder reports the gap; the supervisor resolves it.

## Candidate-builder mode

Required workflow when the environment supports sub-agents. This is a
**cooperative docs-quality discipline, not a security boundary**: it
surfaces gaps by having one role build and verify a real castle from the
same documented surfaces a human castle author should be able to use.

For single-castle work, use this mode whenever practical. For batch
work, it is mandatory on the representative gate set described near the
top of this skill, and every retained castle should come from a
builder-owned candidate unless the user explicitly approves another
workflow.

1. **The `build-castle` invocation is the supervisor.** It reads
   `AGENTS.md`, `test-data/castles/README.md`, and any trigger-topic
   files it needs. It owns repo-rule compliance, bug discipline, docs
   updates, final review, retain/reject decisions, and integration.
2. **The supervisor scopes the castle.** It surveys `test-data/poc/` up
   front, notes any POC package worth depending on, inspects existing
   castles and nearby corpora enough to choose a diversity target, and
   writes a short authoring brief. The brief contains only: the desired
   castle domain or coverage gap; the required output files and
   success-only `run.args` contract including exact protocol tokens;
   whether `input.stdin` is required; the canonical runner host surface
   and module declarations the chosen protocol makes available; the
   protocol's export set / entry point; any reusable dependency surface
   the builder should import; generated POC Kiodoc markdown when a POC
   dependency is in scope; whether imported elaborator forms such as
   `match!` / `widen_sum!` are expected; and the exact verification
   commands the builder should run.
3. **Check the brief before spawning.** The desired program shape and
   chosen protocol must agree: do not ask for string slicing, arrays,
   stdin, or recursive `rec(loop)` flow unless the supplied host surface
   includes the corresponding declarations. Iterative walks, including
   mutable-array updates, default to `rec(loop)`; a direct `loop(...)`
   call belongs only in a castle whose subject is the host loop function
   itself or a higher-order loop combinator. The package's host boundary
   must match the selected protocol's exact inventory, qualified identities
   and signatures; the runner does not discover or filter them from the
   artifact. Supply that inventory in the brief. If a smaller host boundary
   is appropriate, select an existing sharpened protocol with that exact
   contract rather than omitting members of a larger one. See the runner
   README's § The protocol model and § Exact main protocols.
   For testapi-conformed protocols, `string_to_int`
   belongs in `testapi/fmt` with the parse/format helpers, not in
   `testapi/text`. If `elab` forms are not expected, say so explicitly and
   do not include `elab.dep.kio` or an `elab/testapi` bridge entry.
   Because the castle corpus treats a declared `read_ascii_line` as a
   stdin-using package, a castle that declares `read_ascii_line` (from
   `testapi-compute` / `testapi-io` or their siblings) requires an
   `input.stdin` fixture. A no-stdin castle selects an exact protocol without
   `read_ascii_line` and omits that declaration.
4. **Spawn the builder** without forking the supervisor's conversation
   context when the tool supports that. Give it a writeable workspace or
   fork for exactly one candidate. For parallel candidates, use isolated
   worktrees or another staging scheme where each builder's corpus root
   contains only complete direct-child castles plus that one candidate.
   Do not let several builders create incomplete direct children under
   the same `test-data/castles/` tree: the castle orchestrator validates
   the whole direct-child corpus before applying a named filter, so one
   incomplete sibling blocks every candidate's harness run. The builder
   may read `docs/`,
   `ci/infra/kio-test-runner-rs/README.md`,
   `test-data/castles/README.md`, `TESTING.md` when needed for command
   shape, and generated POC package docs from `kio doc build --md`. It
   must not read `AGENTS.md`, `ai/**`, `specs/**`, `kio-rs/`, raw POC
   source, existing goldens, supervisor scratchpads, or the supervisor's
   private conclusions. (The skill system may require the builder to
   load this `SKILL.md` before starting; that read is the only `ai/**`
   exception and does not license browsing other `ai/**` files.) This
   step is the single statement of the builder's allowed / forbidden
   reading sets — other sections cite it rather than restating it.
5. **The builder** creates the candidate source and fixtures, runs
   `kio fmt <workdir>` to canonicalize the candidate (in-place;
   `--check` is the read-only form), then
   runs `kio check` and `kio build <target>` **from the candidate's
   package `workdir`**. It invokes the selected backend's runner with
   `run.args`, and runs the named castle case through
   `ci/checks/orchestrators/castle-tests.sh` when practical. It hands
   back the full candidate plus exact commands run, outcomes, and every
   point where documentation was missing, ambiguous, or misleading.
   The builder does **not** count missing corpus-runner mechanics as a
   public-doc gap when the supervisor brief supplied them. If the brief
   itself is unclear, that is a skill / corpus-contract issue for the
   supervisor to fix.

   Make the verification commands executable in the builder workspace.
   If `kio` is not expected to be on `PATH`, the supervisor should
   provide a `KIO_BIN` path or export `PATH` before spawning. Using a
   prebuilt `kio` binary is not reading compiler implementation source;
   do not require the builder to inspect `kio-rs/` to discover how to
   run it.
6. **The supervisor** reviews the candidate. Retain it only if the
   program is coherent, natural, diverse, success-only, and verified.
   Reject it if it is template padding, a workaround, too thin, or
   insufficiently documented, or if any checked-in candidate file names
   an absolute checkout path, temp path, home-directory path, local
   `file://` URL, or sibling worktree. Request a builder revision for
   non-trivial source fixes.
   Update durable guidance when the builder exposed a real docs gap,
   applying the § Doc-update discipline tier. If the candidate exposes a
   Kio bug, follow workflow step 10 before retaining it.
7. **The final report** states whether candidate-builder mode ran, which
   candidate shapes it covered, what docs gaps it found, which guidance
   files were updated, which candidates were retained/rejected, and
   whether any planned castle family was blocked by documentation or
   runner protocol issues.

Without execution-environment sandboxing this is not hard isolation:
treat it as cooperative authoring. The builder reports what the allowed
docs made possible; the supervisor enforces the repository contract.
When sub-agents are unavailable, do not start a large batch. For a
single castle, the supervisor may do the work only if it reports that
candidate-builder isolation was unavailable and follows the same
allowed-docs-first discipline.

## Doc-update discipline

Castle-building stresses ordinary Kio authoring. If the language rules,
package layout, host surface, runner behavior, or test conventions are
unclear enough that you had to reverse-engineer them, that is a repo
guidance gap, and you fix the appropriate **existing** home in the same
change — you do not restate the rules here. Runner protocol behavior
belongs in
[`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md);
castle corpus conventions belong in
[`test-data/castles/README.md`](../../../test-data/castles/README.md).
A public docs edit must be a general user-facing improvement, not a
castle-specific recipe, corpus procedure, or note that only makes sense
inside `test-data/castles/`.

This skill **defers to**
[`ai/topics/kio-authoring.md`](../../topics/kio-authoring.md)
§ Improving this guide for the whole loop: decide whether the insight is
guide-worthy (non-obvious, reusable, about the language/workflow, **not a
compiler bug** — a bug is surfaced and fixed, never taught around),
verify it with a spec citation or a working golden/example, route it to
the right home, keep it context-free (no-leak), and let
[`audit-kio-guide`](../audit-kio-guide/SKILL.md) backstop the pointer
discipline.

The **blast-radius tier** lives in that protocol's step 4 (its durable
home, so every authoring path inherits one gate). Folded in:

- **Land in the worktree (autonomous):** sharpening a pointer in
  `ai/topics/`, and a small `docs/guides/` addition — a clarifying
  sentence or example — once verified by a spec citation or a working
  example.
- **Surface as a proposal (do not land autonomously):** a new
  `docs/guides/` page, a large rewrite or restructuring of one, and
  **any `specs/` edit** (specs are a contract — "language changes move
  the whole surface together"). Write the proposed change up concretely
  in your final note so the user can approve it quickly; large
  *suggestions* are encouraged, autonomous large *edits* are not.
- The step-4 **confidence brake** still applies on top: when unsure,
  surface the insight in your report regardless of size.

Other directives this skill points at rather than restating:

- [`AGENTS.md`](../../../AGENTS.md) § Trigger table governs which topic
  to read before editing `specs/`, `docs/`, `.kio` files, `kio-rs/`, or
  backend emitters.
- [`AGENTS.md`](../../../AGENTS.md) § Universal rules govern no partial
  implementations, bug-surfacing, natural goldens, surface-form
  lowering, and per-backend limitation discipline.
- [`test-data/castles/README.md`](../../../test-data/castles/README.md)
  is the home only when the unclear point is the castle corpus
  convention itself, not a general Kio authoring rule.

## Diversity lens

A castle earns its place by adding coverage no existing castle has. Keep
this lens lightweight — it makes novelty concrete without forcing a
fixed taxonomy into checked-in castle metadata. Name where the new
castle sits on each axis and how that differs from the corpus:

- **domain** — game, algorithm, utility, simulation, parser, planner,
  data-structure-heavy program, symbolic evaluator;
- **data shape** — products, sums, nested tags, arrays, lists,
  dictionaries (as they become available);
- **execution shape** — command replay, seeded generation, search,
  dynamic programming, tree walk, state machine;
- **host surface** — fixture input, fixture seeds, the string-inspection
  helpers, printing, arrays, numeric roles, string operations;
- **compiler stress** — inference, imports, dispatch, surface forms,
  backend emission, structural recovery.

A new castle should move the corpus along at least one axis in a way the
existing castles do not. State which, and why, in the README's "What
this adds to the corpus" paragraph and in your final note.

## Batches

When the user asks for many castles, treat the request as corpus
curation. Diversity is assessed across the batch as well as per castle,
and the batch discipline is:

- **Plan the distribution up front.** Before implementing, write a
  batch plan allocating candidates across size tiers, architecture
  families, host protocols, stdin/no-stdin execution, and POC reuse.
  Spread the work across architecture families — parser/interpreter,
  graph or search, dynamic programming, symbolic simplifier/evaluator,
  game/state machine, scheduler/planner, data-structure-heavy
  workflows, text scanner/parser, stdin replay, POC-library
  integration — a healthy corpus has different program shapes, not just
  different nouns, and for a sizable batch architecture diversity is an
  acceptance gate, not an aspiration. Compact castles around a few
  modules are useful, but the corpus also needs medium hand-shaped
  projects and genuinely large ones — some hundreds of lines, some
  thousands when the project shape earns that size through real
  modules, algorithms, parsing, state, fixtures, or library
  composition. Host-protocol coverage is secondary: a different runner
  protocol is not a different architecture when the program skeleton is
  the same.
- **Template families are capped.** A repeated skeleton with changed
  constants, nouns, or fixture data may contribute at most a small
  fraction of a batch unless the variants differ in control flow, data
  model, module shape, and compiler stress. The easiest shape to
  overproduce is a compact fixed fixture plus records plus report;
  treat it as a capped family within each tranche, and when several
  recent candidates share a shape, schedule a different execution shape
  next. Do not inflate line count with copy-paste, fixture padding, or
  decorative layers; a large castle must be large because it is doing
  more coherent work.
- **Run balance checkpoints.** At batch milestones, count the live
  corpus by architecture family, runner protocol, stdin shape,
  dependency use, and owned-source size tier, and include the counts
  and a short family table in the supervisor notes. Bias the remaining
  assignments toward underrepresented shapes unless the user asked for
  a specific area — the next accepted candidates should normally come
  from weaker families, not the dominant one — and let the final
  tranche's brief say which gap each candidate fills. Do not let
  "finish the count" override balance.
- **Audit before calling the batch done.** Compare the actual
  distribution to the plan and call out any imbalance rather than
  letting a generated family dominate silently; if one skeleton
  dominates, discard or replace castles until the distribution is
  healthy. If the requested number of meaningful castles cannot be
  built under these constraints, stop at the meaningful subset and
  report the shortfall rather than padding the corpus.
- **Scripts don't write castles.** Batch scripts may track assignments,
  collect builder reports, run checks, or perform mechanical cleanup
  inside an accepted candidate; they must not synthesize castle
  programs in place of builder-owned attempts. Do not accept a batch
  built by renaming one skeleton, and do not continue a batch while
  candidate findings or protocol-documentation questions are
  unresolved.

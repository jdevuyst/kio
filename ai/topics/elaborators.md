# Elaborator implementation boundaries

On-demand reference for the internal architecture of Kio-authored elaborators.
The public language contract lives in `specs/language.md` and
`specs/formal/elaboration.md`; this page explains the implementation constraints
that keep compile-time execution pure, open-world, and phase-local.

## Elaborator evaluation is pure

An elaborator declaration has no configurable purity bit. Its implementation
always runs in the compiler's pure primitive environment. A named
implementation and every ordinary helper reachable from it are `pure fn`s.
Captures are quoted runtime dependencies that the generated term may retain;
capturing a host value does not execute it during elaboration.

The evaluator reduces a prepared Kio'-shaped term plus an explicit primitive
environment. It does not read or mutate the live type solver, retained-source
table, scheduler, module declaration pool, or another ambient compiler state.
Given the same validated implementation, captures, authenticated entry
snapshot, and arguments, evaluation produces the same result independently of
cache history, producer readiness, or typechecker scheduling.

This is load-bearing rather than descriptive style. Normalization may erase,
duplicate, or reorder pure subterms. Prepared evaluator artifacts and completed
results may be reused. A hidden solver write would make an unused expression,
an abandoned branch, a cache hit, or evaluation order change the typechecking
result. Making that write idempotent does not repair conflicts, partial failure,
or rollback; it only makes one repeated successful write harmless.

## Fill state is explicit persistent data

`impl(fills)` receives an authenticated call-local fill context. `__fill__`
appends one relation and returns a descendant context; it does not update the
solver. Discarding the returned value discards that relation. Kio code has no
operation for reading the transcript, producer table, or live goal state.

After the implementation returns, the outer typer authenticates the returned
context, freezes its authored relation order, completes the retained sources
allowed by that transcript, validates the checked term, and publishes the
result atomically. Failure rolls back the whole marked call. Fill contexts and
projected values are call-local capabilities: they are not serializable,
shareable across invocation seals, cache keys, or persistent Prime values.

This division keeps both forms of purity intact:

- Kio evaluation is referentially transparent over its explicit inputs.
- Typechecker mutation happens only in the owner that can validate and roll it
  back transactionally.

## Observation and scheduling boundary

Reflected and projected values are immutable authenticated snapshots. A pure
observer may inspect only the structure present in the supplied snapshot; it
cannot advance a retained producer or make newly solved structure visible by
consulting ambient state during the same evaluation.

Source preparation and completion therefore remain typer operations. Any
boundary that lets an implementation request additional compiler work must
represent that request explicitly in the implementation's input or result;
hiding producer advancement behind an ordinary pure observation would make
dead-code elimination and reordering semantically observable.

The elaborator API has no such request value. Every computed input must have a
determined type before the implementation may observe it, including inputs
with no retained producer work. Explicit product components are checked
recursively; direct lambdas retain only the open final-result exception admitted
by the marked-source preflight. An ordinary `let` completes ordinary body
checking but does not choose unconstrained generic arguments. A complete binding
annotation, explicit type argument, or concrete callback result annotation can
supply missing information where appropriate. The evaluator does not turn an
incomplete inline source into a demand implicitly.

For an ordinary producer call, a fully written callback signature may complete
its result through the already-selected public argument equation. The same
ordinary driver may export a closed result to an enclosing call while keeping
the callback body pending. This is preparation before the immutable snapshot,
not evaluator-driven work. Missing annotations, callback bodies and nested
elaborator actions do not supply information in that header-only pass; the
retained bodies must still check before atomic publication.

Open-world follows from the same boundary. Evaluation and later adoption use
only the resolved call, explicit captures, authenticated structural
descendants, and the finite returned relation transcript. They never search
for a declaration by spelling or scan an ambient candidate pool, so adding an
unrelated declaration cannot change an existing call.

## Phase and cache boundary

Elaborator implementations, projected values, fill contexts, retained
producers, and evaluator control state are transient Lowered-phase machinery.
They are consumed before persistent Prime. The substituted checked term is
then validated from its own representation; downstream phases do not inherit
authority from the elaborator that produced it.

Cache only validated prepared artifacts and completed pure results under their
exact package-analysis identity. Never cache a partial evaluation, an
invocation capability, an unfinished fill transcript, or solver-dependent
readiness. A cache hit may skip computation, so any semantic effect required
for correctness must be represented in the cached completed result and
revalidated at its receiving boundary.

## Implementation checklist

When changing elaborator evaluation, retained sources, reflection, fills, or
their caches, verify all of the following:

- the evaluator receives immutable explicit inputs and no typer callback;
- dropping or duplicating a pure subexpression cannot change solver state;
- every fill relation is present only in the returned descendant context;
- producer execution and goal mutation stay outside evaluator callbacks;
- invocation capabilities cannot enter memo keys, serialization, or Prime;
- adoption validates the exact invocation seal, relation endpoints, checked
  result, and source closures before one atomic publication;
- failure and cancellation discard all unpublished state; and
- authority derives from resolved typed structure, never an ordinary library
  name, producer provenance, or ambient declaration search.

Primary implementation anchors are
`kio-rs/src/pass/typecheck_core/apply/fills.rs`,
`kio-rs/src/pass/typecheck_full.rs`, `kio-rs/src/normalization.rs`, and
`kio-rs/src/pass/substitute/`. The broader phase, application-inference,
artifact-authority, and cache rules remain in `ai/topics/implementation.md`.

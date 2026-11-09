<!-- markdownlint-disable-next-line MD033 -->
<a id="macro-implementation-boundaries"></a>

# Macro implementation boundaries

On-demand reference for the internal architecture of Kio-authored macros.
The public language contract lives in `specs/language.md` and
`specs/formal/elaboration.md`; this page explains the implementation constraints
that keep compile-time execution pure, open-world, and phase-local.

<!-- markdownlint-disable-next-line MD033 -->
<a id="macro-evaluation-is-pure"></a>

## Macro evaluation is pure

A macro declaration has no configurable purity bit. Its implementation
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

## Semantic function reflection

Function reflection observes the logical domain and result. A declaration with
parameters `a: A, b: B` and one with `pair: A & B` expose the same function type;
reflection cannot recover which parameter list was written. Type constructors
likewise do not offer a value-parameter-layout selector. This is separate from
type-constructor kinds and their arities.

Retain private layout metadata only where body binding, generated functions or
host-interface adaptation need it. Validate its layout without letting it alter
semantic type observations or duplicate evaluation of an argument packet.
Removing a reflection operation includes tracing its fields, dispatch and
helpers to actual remaining consumers; neither an internal field nor an unused
public operation is dead merely because current library code does not read it.
The public boundary is specified in [the macro contract](../../specs/language.md#macros-are-imported-not-ambient).

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

Distinguish a rigid generic variable from unresolved inference information.
Each observer requires the information needed for its specified answer: a
determined outer constructor may expose opaque children, while comparing whole
types needs their complete structure. Transporting an opaque handle or adding
a type constraint is not itself an observation of the missing structure.
Follow the operation's contract rather than imposing whole-type completeness
on every observer.

Opened binders of equivalent complete generic types correspond structurally,
including exact free environments and binder positions. Sharing, repeated
views and pure reconstruction produce the same comparison result. The same
rule holds after `__type_var__` conversion. Comparing an opened binder can need
its containing scheme; unresolved structure there fails instead of producing a
boolean. Do not impose that requirement on transport or on observers needing
only a known outer head.

`__type_forall__` and `__term_type_fn__` check their fixed Type or checked-term
result for free references opened from other schemes. Inspect actual occurrences,
including type-bearing checked nodes; unused retained context is not an
occurrence. A whole nested scheme binds its own variables. New and live
enclosing constructor variables and genuine caller-free variables are allowed;
ordinary generic helper substitution preserves the substituted references and
cannot launder an opened variable. A missing region must be proved safe by its
explicit environment or fail for missing information. Opened fragments may
otherwise be inspected, stored and transported; do not enforce this rule over
arbitrary accumulator or closure results. Instantiating the original scheme
with a chosen local variable is the explicit rebinding route.

An evaluated observation that needs missing information fails the macro
expansion. It must not return a recoverable diagnostic or a normal mismatch
that library code can branch on. Discarding the operation's result cannot erase
a failure that already occurred, and memoization cannot hide it. An unevaluated
branch makes no observation; a mismatch between sufficiently determined types
remains an ordinary result. These rules do not authorize an observer to resume
inference or a retained producer.

Source preparation and completion therefore remain typer operations. Any
boundary that lets an implementation request additional compiler work must
represent that request explicitly in the implementation's input or result;
hiding producer advancement behind an ordinary pure observation would make
dead-code elimination and reordering semantically observable.

The macro API has no such request value. Every computed input must have a
determined type before the implementation may observe it, including inputs
with no retained producer work. Explicit product components are checked
recursively; direct lambdas retain only the open final-result exception admitted
by the marked-source preflight. Before freezing inputs, the typer completes an
open computed source through its existing ordinary derivation using available
public type information. An independently typable inner call or macro therefore
need not be extracted into a `let`. Information produced only by the outer action
still requires an annotation or a different composition. This finite source
completion does not turn a reflected observation into a request for typer work.

For an ordinary producer call, a fully written callback signature may complete
its result through the already-selected public argument equation. The same
ordinary driver may export a closed result to an enclosing call while keeping
the callback body pending. This is preparation before the immutable snapshot,
not evaluator-driven work. Missing annotations, callback bodies and nested
macro actions do not supply information in that header-only pass; the
retained bodies must still check before atomic publication.

Open-world follows from the same boundary. Evaluation and later adoption use
only the resolved call, explicit captures, authenticated structural
descendants, and the finite returned relation transcript. They never search
for a declaration by spelling or scan an ambient candidate pool, so adding an
unrelated declaration cannot change an existing call.

## Phase and cache boundary

Macro implementations, projected values, fill contexts, retained
producers, and evaluator control state are transient Lowered-phase machinery.
They are consumed before persistent Prime. The substituted checked term is
then validated from its own representation; downstream phases do not inherit
authority from the macro that produced it.

Cache only validated prepared artifacts and completed pure results under their
exact package-analysis identity. Never cache a partial evaluation, an
invocation capability, an unfinished fill transcript, or solver-dependent
readiness. A cache hit may skip computation, so any semantic effect required
for correctness must be represented in the cached completed result and
revalidated at its receiving boundary.

## Implementation checklist

When changing macro evaluation, retained sources, reflection, fills, or
their caches, verify all of the following:

- the evaluator receives immutable explicit inputs and no typer callback;
- missing information cannot escape as a recoverable observer answer;
- corresponding opened binders of equivalent complete schemes compare equally
  through views, substitution and caches, independent of allocation history;
- quantified constructor results reject actual free opened-scheme references,
  while intact nested schemes and live lexical variables remain usable;
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

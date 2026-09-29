# Implementation

Trigger: editing under `kio-rs/`.

- The compiler/transpiler is written in **Rust** and lives at [`kio-rs/`](../../kio-rs/) — package name `kio-rs`, two binaries from one crate: `kio` (full Kio compiler) and `kio-prime` (Kio'-only compiler). Both binaries dispatch through the shared `Pipeline` trait. `kio` selects `FullPipeline`: package block-declaration validation, `op_fold`, `block_projection`, `desugar`, and `label_elab` produce `Lowered`; name resolution and the elaboration-aware checker produce a checked Lowered tree plus `typecheck_full::Elaborations`; `substitute` consumes those records into Prime; and the standalone Prime checker validates and canonicalizes the result. `kio-prime` selects `PrimePipeline`: `prime::lower` rejects surface-only forms while converting parsed Surface directly to Prime, then the same standalone checker validates the input, bakes its node-local call/lambda completions and typed intrinsic-value wrappers into the AST, and canonicalizes it. `Package::build`, phase-polymorphic resolution, and the synthesis, checking, application, module, and environment machinery under `typecheck_core` are shared. The phase-specific pieces are the Lowered-only elaboration arms and `typecheck_full::Elaborations`, plus `prime::typer::PrimeTyper` and its narrower `PrimeElaborations`; surface-bearing variants are uninhabited at Prime. Both completion tables are consumed before their phase boundary closes. Cargo features (`surface`, `prime`, both default-on) gate the full-Kio modules (`desugar`, `elaborator_registry`, `full`, `label_elab`, `op_fold`, `substitute`, `typecheck_full`) and the Kio'-only modules (`prime::lower`, `prime::walk`, `prime::pipeline`); `prime::typer` and `prime::canonical` are shared so full Kio can validate and canonicalize substituted Prime. CI builds each binary with only its own feature, catching cross-binary leakage at compile time.
- The kio-prime binary is wired into the CI golden runner as a separate `--impl-def=` over `IS_KIO_PRIME`-marked cases (see `ci/checks/orchestrators/golden-tests.sh`).
- Standard Rust tooling applies; from inside `kio-rs/`, invoke it through the repository wrapper (`sh ../ci/cargo.sh build`, `test`, `fmt`, or `clippy`) so concurrent work shares the configured scheduler resources.

## Phase-boundary purity

The elaborator-specific purity, immutable-fill, retained-source, and cache
boundaries are collected in [`elaborators.md`](elaborators.md). The rules below
place those constraints in the complete compiler pipeline.

The forms whose removal needs completed typing information — imported elaborator calls (including `if!`, `scope!`, `do!`, and `match!`), field access/update, and UFCS normalization — use the boundary-local `typecheck_full::Elaborations` table and are swapped into the AST by the Lowered → Prime substitute pass. That pass also materializes inferred literal annotations, call arguments, implicit Unit slots, and checked lambda parameter types. Earlier front-end passes remove other surface syntax: `op_fold` rewrites operator chains; package-scoped `block_projection` projects generic trailing blocks according to the resolved ordinary elaborator declaration's product, thunk, or sequence descriptors before `desugar`; `desugar` removes tuples, placeholder lambdas, recursive-call syntax, and literal aliases, using its boundary-local `RecOrder` carrier where recursive continuation ordering needs checking; label elaboration removes label declarations and label-value/type sugar. Sequence projection builds ordinary nested bind calls inside a function; the imported `do!` elaborator calls that function with the receiver, evaluating the receiver expression exactly once. Neither the projection nor typing grants a primitive by the spelling of an ordinary library declaration. `RecOrder` and recorded elaborations are consumed before Prime. The standalone Prime checker uses its narrower `PrimeElaborations` table for Kio'-admitted call/lambda completion and already-checked intrinsic-value schemes, then bakes those records before returning. Post-typecheck passes that stay inside strict Kio' (`prime::canonical`, `structural_recovery`, `recover_to_low`, the `capabilities::annotate_*` suite) introduce no surface forms and don't bear on the boundary rule. Scheduled cache misses run `prime::canonical` after completion baking and before storing a typed module; the assembled package then runs standalone Prime validation and the idempotent canonicalizer again. Package-level typecheck and defensive Kio' emission likewise leave both direct compilation and emitted/reparsed Kio' with the same capture-safe statement spine.

Intrinsic values use the same standalone checking boundary. A bare intrinsic or
a completed type-only application records its exact checked scheme; a saturated
direct application records nothing, including when its result is callable. The
existing bake walk materializes an ordinary fully annotated function with fresh
lexical binders and a saturated intrinsic call. It retains the original selected
Bool-role identity rather than resolving a role again. The optional occurrence
map is consumed before returning Prime; emitted, cached and reparsed artifacts
contain only ordinary functions and explicit types. Revalidation introduces no
additional wrappers.

### Label identity across the Surface boundary

An explicit label entry introduces two linked Surface facts but only one nominal
identity. For `labels { foo: P };`, `Foo` is the generated nominal type and
`{foo}` is a separate label-namespace binding that points to that exact nominal;
the lowercase binding is neither a value alias nor another nominal. Label
elaboration resolves braced construction, access, update, and imports through
that explicit binding, lowers them to ordinary uses of `Foo` and its `mk` / `get`
members, and removes the binding before Kio'.

Do not reconstruct a label binding from capitalization, a same-named transparent
alias, member spellings, or producer provenance. Conversely, do not treat the
fact that `labels` is sugar for generated newtypes as making every ordinary
`newtype` a label declaration. A transformation that promises to preserve a
module's Surface export contract must preserve both an exported label binding
and the exact nominal identity it targets without minting a replacement. If the
ordinary Surface representation cannot express that result, stop at the design
boundary rather than using a hidden sidecar or a compiler-generated privilege.
The language contract and lowering inventory are in
[`specs/language.md` § Labels](../../specs/language.md#labels) and
[`specs/prime.md` § What's not in Kio'](../../specs/prime.md#whats-not-in-kio).

Recursive CPS lowering uses one narrow boundary-local `Expr::RecOrder` carrier. `desugar` creates it only at `Desugared` and `Lowered` through `Phase::ExprRecOrder`, with one pending value and one already-built continuation body. The `Ordered` disposition retains three flows: `ExpectedFromBody` checks the continuation body first and obtains the pending value's exact type from the generated binder's classified use; `SynthesizedValue` synthesizes the pending value first, validates any partial source `let` annotation against that exact type, and exposes the type while checking the body; and `ElaboratedValue` checks the pending value at its ordinary source type while exposing its separately validated elaborated runtime type to the continuation binding. An exact compiler-generated `ExpectedFromBody` shape carries an annotation `(_input) -> runtime_ty`: after structural preflight, the ordinary driver reserves one input goal in that carrier's live owner delta and exposes the resulting function shell as a normal local value while checking the body. If the original continuation is an already-bound lexical path, its exact retained type is transported to that owner; a goal-free ordinary function header constrains the shell before the body starts. Open headers, function schemes, and incomplete lambda continuations retain body-first inference. Direct calls and ordinary value occurrences of the local constrain that one goal through normal typing. The input goal must be closed when the body completes; after its scope unwinds, the driver checks the sole original continuation against the same now-concrete shell. Other `ExpectedFromBody` annotations are an internal invariant failure; the legacy annotation-free pending-binding path is unchanged. A value-shaped type argument classified while checking an expected-flow body instead records a type-only disposition. Continuations with a fully concrete parameter type remain ordinary calls; in particular, expression sequencing gives its generated parameter the exact Unit type.

For recursive-source lambdas staged through a marked elaborator, `DeferredTail` privately records a `Required` or `Optional` candidate and the ordered flow used if the candidate is lifted. Recursive CPS lowering binds the sole unknown-input continuation with the exact function-shell carrier above. Hoisting one continuation subtree and passing only its generated path through every candidate is a private generated-tree/layout invariant, not source-language behavior; the continuation subtree is never cloned beneath candidate binders. The rare unknown-input typer state keeps the hot `RecOrderContinuation` size unchanged and costs one reusable box, one required owner-local goal, and one interned shell per active carrier. After result validation, the typer accepts only a checked recipe sealed for that invocation as structural evidence, then traces each candidate's exact `TemplateValue` identity through pairs and projections, lexical `let` aliases, and type specialization. `Either` and `If` propagate tail position into their branches; opaque subterms and non-tail uses mark escapes. Every candidate is classified successfully before any resolution is installed. A terminal-only candidate lifts with its recorded flow; an `Optional` candidate with no terminal use bypasses the wrapper, while a candidate with both terminal and escaping uses or a `Required` candidate without a terminal-only proof is rejected. This proof follows structural recipe identities and exact carrier tokens, never declaration spelling, library identity, or raw argument indices.

Materializing a classified lifted callback walks the authenticated checked-term recipe and retypes every occurrence of its exact template identity. The walk keeps subtree change propagation separate from result selection: an optional rebuilt `Arc` records whether any descendant changed, while copyable symbols in an invocation-local pair arena record only whether the node's result is public, is the selected lifted callback, or retains product-selection topology. Thus an opaque call argument, scrutinee, condition, function body, injection, or absurd input rebuilds each affected ancestor without falsely turning the enclosing public result into a selected callback. Lexical binders restore their prior symbol mapping, unchanged children remain shared, and one pair occurrence allocates at most one arena node. Since classifier success authenticates this topology before materialization, a missing template slot or a classifier/materializer disagreement is an internal invariant failure rather than a source diagnostic. The arena and rebuilt-term state are transient and are consumed before substitution closes the Lowered → Prime boundary.

`substitute` materializes one typed ordinary `let` only for a runtime value, substitutes the recorded type-only body or bypassed value otherwise, and removes every carrier before Prime. `RecOrder` is neither a surface elaborator nor a Kio' form. The parser, formatter, Kio' verifier grammar, normalizer, generator, syntax tooling, LSP, and `kio sig` therefore gain no new spelling or contract surface. Ordering, deferred-tail classification, and type-flow selection depend only on the local expression and checked recipe structure, so adding declarations to any module cannot change them.

An `Optional` carrier reached inside an ordinary computed source, outside the
elaborator's staged callback slots, also consumes its ordinary value through
the existing bypass. Its enclosing expected type reaches the original value
before checking; a written return annotation retains ordinary annotation
checking. The retained child completes once and publishes the checked value
only on success. This grants no recursive permission: unselected `Required`
carriers remain errors, and recipe-selected lifting retains its proof.

### Retained application inference

The full-language typer may retain an unfinished immediate argument when its
parameter slot depends on other information in the same written call tree.
This remains ordinary synthesize/check typing: a retained source action is the
unconsumed edge of one derivation, not a provisional success and not a third
mode.

One finite inference domain covers a connected call-local problem. A nested
call whose unfinished result participates in its ancestor's inference joins
that outer domain, while each call retains a distinct lexical owner for its
binders, skolems, goals, and retained actions. A nested call that completes
independently uses its own domain and contributes only its closed result.
Never copy an unresolved nested goal into another owner, a cache key, a
reflected value, or a later phase.

The schedule is fixed and one-shot:

1. Establish or join the finite domain, allocate owner-scoped goals only for
   written omissions, apply explicit type arguments, and constrain any
   expected result. Each application owner may retain at most one pending
   expected-result equation. After an initial blocked attempt, retry it only
   when a retained child closes or one of its zonked sides changes
   structurally; record a new snapshot if that attempt also defers. A schedule
   mode change alone is not progress.
2. Enter each immediate value expression fresh once. Complete what is
   independently typable; retain only the unconsumed source action of a
   blocked value.
3. Run the expected-only pass left to right. Consume an action whose parameter
   slot has become usable; do not use lexical literal fallback in this pass.
4. Run the lexical-fallback pass for still-untyped literals.
5. After marked source recipes are snapshotted, run the `impl(fills)` source
   eligibility preflight before its action. Traverse every declared source and
   recipe in structural order, including ignored private slots and explicit
   product components. Every computed leaf must have a closed type, regardless
   of retained-producer state. Available expected/sibling public constraints
   may close it before this check, but later fills and body checks cannot.
   Declared generics are closed types, not unsolved goals.
   A direct Lambda may remain open only when its exact
   signature peel proves a complete `forall` and value-parameter prefix, no
   `Infer`, and all remaining `Goal` occurrences confined to the final body
   result. Reject the first other open value source at its source
   span before action execution, body resumption, or publication. This check is
   a linear staged-to-recipe zip and does not execute or solve through a recipe.
   Before snapshotting an ordinary call, its existing driver may contribute a
   callback's complete written signature to the already-selected expectation.
   This header-only pass suspends bodies, immediate-lambda callees, statement
   prefixes and nested elaborator actions. A goal-free pending call result may
   use the existing exact-parent header export without closing its child or
   publishing facts; every retained body still finishes in the ordinary pass.
6. Run a final preflight that proves every remaining slot and terminal state
   can close, without executing a retained source action.
7. Run the final pass left to right, reuse completed derivations, consume each
   remaining action at most once, and require every owner-scoped goal to close.

There is no retry, source-premise or typing-derivation replay, backtracking,
global worklist, or fixed-point iteration. A lambda body may contribute a
result type that is independently available, but it never discovers a missing
parameter type from uses of that parameter. A `let` or expression-statement
prefix finishes before its tail; only the tail receives the enclosing expected
type. Later uses must not infer an earlier expression-block source's missing
type or a sequence action's payload; ordinary bind carrier/result constraints
remain available. Semicolons separate clauses, and a trailing separator leaves the final
result unchanged; the separator itself is not a universal inference barrier
for every block descriptor. Marked `impl(fills)`
staging may retain a lambda body after its structural header is available. A
directly synthesized lambda with explicit type binders and a complete written
parameter header may reserve an omitted or `_` final result in its ambient
premise owner, outside the newly introduced binders. Prepared header export
authenticates the exact child delta and follows every ordinary direct-parent
scope check. The action may observe this open result, but an independent
expectation or finite fill relation must determine it before the one generic
body enters checking. An internal open header, including a lifted recursive
header, cannot establish that anchor. A new binder-dependent result requires
the existing concrete-annotation or complete-expected-function route.
`match!` follows this generic schedule through its ordinary result fills, not
a special clause inference rule. Named or already-typed polymorphic values
remain admissible when their types are closed; ordinary synthesis and top-level
unit return defaults are unchanged.

The shared surface-call selector is lexical and greedy. After pending type
slots, every non-final written value consumes one right-spine product slot;
only the final value before a type argument, explicit nested call, or end of
the list may fill the remaining product packet. Do not probe an argument's
type, search suffix fits, or retry a different packet split. Ordinary, bang,
and every UFCS direction feed this one selector. At a UFCS receiver boundary,
reserve type-looking candidates for the consecutive binder run immediately
after the receiver; the adjacent leading binder run consumes only the surplus.
An unfilled value layer is a hard boundary, so reservation never crosses it.
The reservation depends only on the resolved call slots and written argument
kinds, never on a candidate's inferred type or ambient declarations.
Here, resolved call slots are the semantic public type after ordinary alias
unfolding and substitution. Signature parameter grouping,
`Type::Function.abi_arity`, callee identity or provenance, and a user
elaborator's private implementation ABI are not call-selection inputs. They
may bind definition bodies, adapt a runtime calling convention, or decide when
an already-selected elaborator action is ready, but they cannot change call
completion, grouping, residualization, or UFCS placement. Counterfactual tests
must give ordinary functions and Required/Optional elaborators with the same
public type the same call result.

#### Written application authority

A syntactically empty direct/prefix surface call list materializes one ordinary
Unit value argument before the phase boundary while omitting leading type
arguments. The written Unit and the call's local constraints must solve those
omissions. Thus `nil()` for `nil : [A] . -> List(A)` succeeds only in a checked
position that solves `A`, and lowers to explicit `nil(A, ())`.

A nonempty packet containing only type arguments or `_` placeholders is
different: it performs only those type applications and leaves the selected
value function residual. Substitution or alias unfolding may expose a Unit
domain, but never authorizes an unwritten Unit application. `identity(_)`
checked against `. -> .` therefore lowers to residual `identity(.)`, while
`nil(A)()` and `nil(A, ())` contain the written Unit application and saturate
the Unit layer. UFCS first inserts its receiver and only then submits the
resulting prefix plan to application checking. The receiver-only spelling
omits its list, so `r.>f` normalizes to `f(r)`; a present list must be nonempty,
and `r.>f(())` normalizes to `f(r, ())`. The parser rejects `r.>f()` before the
omitted/present distinction could be erased. `().>nil(A)` normalizes to
`nil(A, ())` and saturates because the receiver itself is Unit. Every selected
type stage remains before its written value stage.

Application and UFCS selection read only the resolved callee type and written
packet. They must never consult declaration arity or grouping, parameter
spelling, `Signature` / `HostFn` fields, named-callee identity, provenance, a
user elaborator's private ABI, or parallel metadata to decide whether a value
application exists. These facts may adapt an already-selected runtime calling
convention, but they cannot change completion or residualization.

The consumer impact is narrow and end to end. The surface parser rejects a
present empty UFCS list before it would collapse to the same AST as omission;
the formatter preserves bare and nonempty forms, including bare left-callee
bang splices. Lowering materializes inferred surface types and the direct
empty call's Unit value. The independent Kio' verifier validates the resulting
explicit type/value application order and never reconstructs an omitted
application. `equiv`, `:normalize`, and `kio-gen-rs` consume the same parsed or
lowered shapes; syntax tooling recognizes all four bare forms; LSP diagnostics
carry both repairs; and `kio sig` gains no host-contract spelling or
independent completion rule.

The open-world argument is direct: the resolved callee type and written call
tree are finite inputs fixed before application. Adding another declaration
cannot alter either input, so it cannot change Unit completion, receiver
placement, or the resulting type.

A user elaborator is evaluated only after every dependency visible through
its reflected ABI is closed. An optional `__Type__ | .` slot receives unit
only when that exact declaration binder occurs in the result and has no value
dependency. A phantom binder, or one shadowed before the result occurrence,
does not qualify. Reflection, memo lookup, and evaluation use those same
closed dependencies. The implementation has one semantic evaluation request;
its returned checked term may solve only result goals allocated before
evaluation, and the term and its inferred type publish atomically. Optional
absence is an action-readiness rule only: it never turns an unsolved written
`_` into a new residual `Forall` or otherwise changes the public call plan.

Every goal and retained action is consumed before Lowered becomes Prime. The
resulting calls carry explicit type arguments, inferred lambda annotations are
concrete where Kio' requires them, and standalone Prime validation never
reconstructs this inference relation. The parser, formatter, Kio' verifier
grammar, generator, syntax highlighting, and backend IR gain no spelling or
phase form. The LSP and `kio sig` consume the same completed typing decisions
as the ordinary front-end.

The open-world proof is structural: the domain is built only from resolved
callee types, written arguments, enclosing expected types, and the lexical
literal-role pool. It never scans an ambient declaration set. Adding an
unrelated module-body declaration therefore cannot alter the domain, its fixed
schedule, or its result.

Phase-boundary purity means each later phase consumes only the artifact declared for that phase. A phase-boundary leak is any surface/history/typer-only fact observable after the boundary closes. Use **boundary-local side channel** for temporary scaffolding consumed before a boundary closes by writing into the next-phase AST; use **cross-boundary side channel** for forbidden parallel data consumed after the boundary instead of being represented in the phase input. Keep **no-leak** / **context leak** vocabulary for session, machine, user, worktree, and scratchpad leakage in checked-in artifacts.

### User declarations are not syntax

A normal Kio declaration remains an ordinary declaration in every compiler
phase. A parser, desugarer, typer, substitute pass, structural-recovery or
`recover_to_low` pass, capability annotator, evaluator, optimizer, or backend
must not grant special typing, lowering, evaluation, control-flow, visibility,
or capability behavior because a user-controlled item has a particular bare
spelling or because its resolved identity equals a compiler-hard-coded or
otherwise distinguished ordinary-library declaration. Replacing a bare-name
comparison with such a fully qualified identity does not repair the design: a
Kio-authored elaborator such as `match!` is still a user declaration, not a
compiler primitive.

Resolved identity itself is ordinary semantic data, not forbidden provenance.
Use it generically wherever the resolved program requires exact identity:
validating a specific capture or visibility edge, comparing nominal types,
ordinary call/member dispatch, linking host declarations, choosing backend
symbols and namespaces, rendering diagnostics, and keying caches. The boundary
is whether the algorithm follows the identity selected by the program, or asks
whether that identity is one compiler-known library item in order to grant it
different semantics.

Parsed argument positions may drive a grammar-defined surface form. For an
ordinary function or elaborator call, however, positional behavior follows the
resolved call type and its semantic slots; a pass must not index raw source
arguments according to a remembered library ABI before ordinary call
resolution. Declaration spelling, parameter grouping, runtime ABI grouping,
and producer provenance are likewise not alternate semantic authorities.

When a library abstraction appears to need compiler cooperation, first seek a
general transformation over the elaborated structure. If structure alone is
insufficient, stop and present an explicit, general language construct or
capability for authorization; do not hide that construct behind a library name.
Reserved intrinsics and genuine grammar forms are admissible only where the
specification explicitly grants their semantics.

Regression evidence for a repaired violation covers both sides of the
counterfactual: an unrelated user declaration with the tempting spelling stays
ordinary; a differently named or rehomed declaration with the same relevant
ordinary structure receives the same general behavior; and the intended
library behavior survives every admitted reference spelling and call direction
without a name hook. If an authorized explicit capability is selected instead,
vary the name while preserving that capability. Where applicable, cover
qualified and selective imports, same-name declarations in distinct modules,
prefix and UFCS calls, and a changed library-local implementation name. A test
that exercises only the canonical library spelling cannot distinguish a
structural implementation from a spelling- or identity-based cheat.

### Artifact authority

Kio' is a strict subset of Kio, not a privileged compiler-output dialect or an extension language. Every construct and spelling accepted by `kio-prime` is accepted with the same meaning by the full Kio pipeline. Lowering may reject or desugar surface forms and synthesize ordinary subset terms; it must never introduce a Prime-only import, identifier class, AST case, or semantic privilege. The meaning of the resulting artifact is identical whether it came from lowering, a fresh `kio` or `kio-prime` parse, a cache round trip, or dynamic loading. A reserved identifier can keep generated names collision-free only when that identifier class itself belongs to the shared subset; neither the spelling nor a claim of compiler origin proves a visibility or capability fact.

### Signature-history provenance

A sealed `*.sig.kio` is compatibility history, never a second live host
contract. Current bridge-selected source builds the live requirement and
execution inventories. Replay may build a disjoint retained-facade inventory,
using the live-dominant provenance join in [`emit.md`](emit.md) § Retained host
declarations. Never reconstruct provenance from generated spelling, and never
let a history-only item enter loader matching, current-host validation, or an
execution-capable facade plan.

Retained closures keep their own frozen nominal declarations. If one exact
identity has incompatible snapshots, the live snapshot wins and roots that
depend on a conflicting retained snapshot are omitted; two conflicting
retained snapshots suppress every dependent retained root. Never merge, widen,
or select an epoch by replay order. The source-compatibility rule and backend
impacts are in [`emit.md`](emit.md) § Retained host declarations.

If an authorized private edge remains represented after the Prime boundary, it must be both narrow and independently checkable from ordinary shared-subset structure in the Prime artifact plus explicit package dependencies. When a public surface declaration selects one private implementation member, any receiving validator must verify only that exact declared relationship; it must not turn the selection into access to the provider module's other private functions or type members. This constraint does not choose a representation or require the edge to cross into Prime: if an intermediate compiler state cannot carry evidence that survives and validates under a fresh textual round trip, consume that state before Prime. Serialized and cached artifacts may memoize a successful validation under their normal integrity/fingerprint contract, but serialization adds no semantic authority. Direct compilation, emitted-and-reparsed Kio', and validated cache reloads must accept and reject the same authority edges. The runtime `dyn_load_prime` boundary deliberately trusts a precompiled image's function bodies and rederives only the documented structural and host/export-contract edges; successful loading is not full artifact validation.

Compile-time evaluation follows the same rule. `equiv`, the REPL `:normalize` command, user elaborators, generated-term templates, and future `__comptime__` hooks must all share one reducer contract: input is a Kio'-shaped core term plus an explicit primitive environment. In kio-rs, eval adapters may start from typed `Lowered` plus the boundary-local `Elaborations` table only long enough to substitute known elaborations into an `UncheckedPrime` term; after that boundary closes, the reducer and its caches must not read `Lowered`, surface AST nodes, `Elaborations`, hidden typer state, or any other cross-boundary side channel. Explicit compile-time holes are represented in the `UncheckedPrime` term itself as residual terms; they are not implicit access to typer state. Entry points may differ in how they prepare a term, compare results, or reify diagnostics, but the reduction semantics belong to the shared evaluator.

### Function purity versus phase-boundary purity

The language's `pure` marker is a declaration-reference contract for an ordinary `fn`; it is not the phase-boundary property named by this section. Only an ordinary function admits the modifier. The checker examines resolved executable value references in its body, including references nested under lambdas, and admits locals, intrinsics, newtype members, and functions whose own contracts carry `pure`. It does not classify types: signatures, reflected type arguments, and local type annotations may mention host types or any other well-formed type. Do not add purity fields to type, newtype, labels, elaborator, operator, fold, literal, equivalence, host, or recursive-group representations.

A user-defined elaborator has no purity bit. Its implementation always executes in the compile-time pure environment. A named implementation and each ordinary function in its transitive helper closure must be a `pure fn`. An implementation declared in the elaborator's defining module may remain private, and private helpers may appear in any implementation's transitive call closure when ordinary same-module visibility permits them. A cross-module target must be ordinarily importable into the elaborator's defining module, but need not meet the elaborator's outward visibility. Captures are different: they are explicitly quoted `__Type__` or `__Checked_term__` runtime dependencies, may name host items, and carry that outward visibility floor because the generated term may retain them. Capturing a host function does not call it during elaboration.

The marker on an ordinary function remains in Kio' long enough for a fresh parser, validated cache load, or emitted-and-reparsed package to reconstruct and validate the contract. An emitted loadable image also retains the marker, but `dyn_load_prime` recognizes only the syntax and does not re-typecheck the trusted body. Elaborator declarations, implementation-path selection, and the compile-time primitive environment do not cross the persistent Prime boundary. Implementation and helper `fn` declarations remain ordinary package declarations, but `RuntimeErase` replaces a function body gated by a `__Comptime__` proof with an ordinary `__absurd__` body and erases compile-time-only types before persistent Prime. After substitution, run standalone Prime validation over the assembled package. That validation, not producer provenance, decides whether a generated host or unmarked-function reference is allowed in its actual caller.

Consumer impact is deliberately narrow but end-to-end. The surface and Kio' parsers, formatter, Kio' verifier, serialization/cache paths, and `kio sig` retain and validate `pure` on ordinary functions and reject it on every other declaration. The runtime dynamic loader must recognize the same grammar and reject invalid modifier placement, but—like all body typing—it discards the marker after scanning rather than adding an independent purity checker. Syntax highlighting and the LSP continue to treat the existing contextual word as a function modifier. `equiv`, `:normalize`, backend IR, and host emitters consume an already validated term and gain no independent purity inference or role. This division keeps direct compilation and every full-validation textual or serialized round trip equivalent without misrepresenting the loader's trust boundary.

`__comptime__`-style functionality is host-provided primitive behavior in that explicit environment. It is not ambient access to builtin-module declarations or compiler internals, and builtin module surface still follows AGENTS.md § Universal rules — Builtin modules are approval-gated.

The open-world argument is: evaluation never performs ambient name lookup over the surrounding module body. Adding declarations to a module can affect compile-time evaluation only when a front-end explicitly builds a different Kio'-shaped term or a different primitive environment. Existing terms continue to reduce against the same term graph and primitive table.

Compiler hardening requirement: the full `kio` path typechecks Lowered, substitutes to Prime, and must run the standalone Prime checker over the assembled substituted package before backend consumption. That checker validates artifact authority as well as types; it must not trust that the full pipeline produced the input. Keep `prime::canonical` after elaboration baking on typed-module-cache and auxiliary-module outputs, and after validation on package outputs; its idempotence makes the repeated package/emitter defense safe. Preserve the `surface` / `prime` feature split.

## Complexity checkpoint for compiler design

A narrow language edge that starts spreading special state or branching across
several compiler phases is a reason to stop and compare designs before adding
more machinery. Report the production-code footprint, any new persistent or
transient state, every phase and consumer that must preserve it, cache and
performance consequences, and the simpler contract or architectural options.
Include deletions and consolidation opportunities: line count alone is not the
measure, but a design that removes states or paths is materially different from
one that only adds another exception.

First verify the authority baseline and the current implementation. If the
contract uniquely requires the behavior, complexity is evidence that the
architecture may need a better representation; it is not permission to narrow
the language. If more than one contract is coherent, present the comparison and
wait for the required decision before implementing one. Passing tests, an
already-large branch, or time spent on it does not settle that decision.

## Generated binding hygiene

Every value, type, or import binding synthesized by a compiler pass or emitted
backend must be capture-proof. The default is a spelling outside its relevant
user-reachable namespace: rejected by the Kio grammar for an IR binding, or
outside the image of legal Kio names under a backend's identifier mapping for
a host binding. Do not mint an ordinary user-reachable name merely because a
prefix looks private.

Treat each namespace according to the language that consumes the binding.
Value locals, type parameters, and nominal or alias owners may occupy separate
namespaces in one representation and overlap in another. A generated import
introduces its identity into the consuming namespace that actually resolves
it: an unqualified import may bind values or types, while a qualified alias may
bind a module or package name. Maintain and seed each actual namespace
independently; merge sets only where the consuming resolution rules merge them.
A collision-free value name is not evidence that the same spelling is safe in
another namespace.

A deterministic collision allocator is a constrained fallback only when no
legal target spelling can be kept outside the image of legal Kio names. For a
host backend, account for the identifier mapping it controls: first determine
whether escaping or mangling every user-derived name can reserve a class for
generated identities in that namespace. Record why neither the target grammar
nor a reserved mapping class is usable, then seed the allocator from the
complete relevant namespace before inserting the binding. A source span,
counter, hash, unusual prefix, or unused-binding convention makes a collision
less likely; none makes it impossible.

For a value allocator, "complete" includes parameters, lets, nested
lambda/case binders, path heads that a rewrite can move into local position,
prior generated names, and free names newly enclosed when a rewrite widens
scope. At item scope it also includes user-derived emitted functions and
values, fixed package/runtime helpers, wrapper methods, and namespace/object
value members. For a type allocator it includes in-scope `forall` and other
type parameters, type-path heads, alias and nominal owners, prior generated
type names, and free type names newly enclosed or substituted. For an identity
introduced by an import, seed the actual consuming namespace with every
authored, imported, and generated identity visible at the emitted use sites,
including aliases and qualifiers where those are what the consuming
representation binds. Backend sets additionally include the user-derived and
fixed runtime names introduced in that same host namespace, including
generated type aliases, nominal owners, and backend type parameters.

The declaration and every bound or qualified reference must use the same
identity without touching a shadowed inner or outer binding. Substitution
beneath `forall A` alpha-freshens `A` before descent when the replacement has
`A` free, renaming the binder and all of its bound occurrences together; this
applies equally when an occurrence is an applied type head. Keep each private
implementation supply scoped to its lexical or emitted owner: declarations
outside that owner cannot perturb its generated spelling. Adding a colliding
user binding inside that owner may select a different private spelling, but
must never change binding or program meaning. A host-visible facade identity
instead follows the stronger stable injective semantic mapping in
[`ai/topics/emit.md`](emit.md) § Package namespace: declaration
occupancy must not rename an existing public selector.

The rule covers local and item-level value bindings; `forall` and backend
generic parameters; generated type aliases, nominal owners, and their bound
uses; and generated imports or import aliases. The same rule applies after
source emission. A generated binding printed by the Kio' backend becomes
ordinary source on a fresh parse, so its hygiene must survive dump → parse →
Prime check without hidden compiler state. A host backend's generated binding
must be disjoint from every user-derived, fixed runtime/helper, and sibling
generated identity in the same host namespace; its rendering must also account
for host keywords and reserved syntax. Compare identities after every
transformation the renderer and host apply — escaping or raw-identifier
decoding, case/Unicode normalization where applicable, truncation, and
mangling — not by raw emitted-string inequality. Property labels and
structural-shape keys used only for lookup are not bindings; a method, type
member, namespace declaration, or other construct that introduces an identity
is.

Evidence is semantic, not prefix-shaped. A collision allocator is tested by
occupying its first candidate in the same namespace under nested shadowing and
asserting declaration/use identity; a reserved spelling is tested against
every legal user-name route into that namespace. Type substitution evidence
forces a free-name collision beneath `forall`, including an applied-head
occurrence. For bindings that reach Kio' source, also cover a fresh-load round
trip; for backend bindings, compile the emitted artifact and exercise the
binding where it is executable. Exact private spellings belong in Rust unit
tests or generator self-tests, not implementation-narrating public goldens.
[`audit-generated-binder-hygiene`](../skills/audit-generated-binder-hygiene/SKILL.md)
inventories the binding sites and checks these proofs.

## Compiler performance architecture

Compiler changes must preserve the performance architecture for both batch commands and LSP workflows. Independent package/module/target/file work fans out through the `maybe_par_iter!` family under the default `parallel` feature; source bodies and LSP syntax data stay lazy until the workflow needs them; `kio check` and `kio build` reuse the Kio-semantic cache layers instead of adding uncached hot paths; LSP analysis stays debounced, backgrounded, cancellable, and able to serve stored full/focused analysis for read-only requests; substantial new hot paths include cache or timing probes so regressions can be diagnosed.

Performance probes are debug-only surfaces (AGENTS.md § Universal rules — Debug-only surfaces are internal); their option-level documentation belongs in agent/internal guidance such as [`local-performance.md`](local-performance.md), not public `specs/` or `docs/`.

When a feature truly needs a serial pass, eager parse, cache bypass, or synchronous LSP analysis, document the ordering/freshness reason in the implementation and add a focused regression guard. When a change adds, removes, or materially changes a performance mechanism, update the inventory below in the same change; update [`audit-compiler-performance`](../skills/audit-compiler-performance/SKILL.md) too when the new mechanism needs a new detection pattern. The audit sweeps for drift.

| Mechanism | Hot path | Anchors |
| --- | --- | --- |
| `parallel` feature + `maybe_par_iter!` family | Independent package, module, target, file, and snippet fan-outs | `kio-rs/Cargo.toml`, `kio-rs/src/par.rs`, `kio-rs/src/cmd/package_fanout.rs`, `kio-rs/src/cmd/check.rs`, `kio-rs/src/cmd/build.rs`, `kio-rs/src/backends/mod.rs` |
| Lazy module parsing and forced bodies | Package walk, cache summary, focused LSP analysis | `kio-rs/src/package_collection.rs`, `kio-rs/src/pass/parser/`, `kio-rs/src/cmd/check.rs` |
| Ordered cancellable focused-module force/lower fan-out | A focused LSP request maps only its existing conservative same-package dependency closure through `maybe_par_iter!`, retains input order, selects the deterministic first error before merging, and never publishes a cancelled or partial batch. The no-`parallel` feature path uses the same collection and publication protocol sequentially | `kio-rs/src/cmd/check.rs`; causal overlap, order, cancellation, first-error, fallback, and measured-win evidence is registered in `audit-compiler-performance` § 8 |
| Package-check cache | Warm `kio check` package skip; warm no-equiv `kio test` fast path (reuses the same entry). Globally and package-locally disabled caches bypass whole-source hashing; `kio build` still hashes and warms every active package cache. Reload validates the exact semantic header/key and encoded-body digest before accepting the cached package summary; a following store atomically repairs an invalid entry | `kio-rs/src/cache/package_check.rs`, `kio-rs/src/cmd/check.rs`, `kio-rs/src/cmd/test.rs` |
| Content-addressed typed-module cache | Warm typecheck within one package and across byte-identical module inputs copied into distinct package roots during one corpus run. The semantic key contains the compiler/cache identity, pipeline, declared module path, exact source, and sorted reachable ordinary-import and elaborator dependency fingerprints; operator, fold, and elaborator implementation paths add no hidden direct-FQN edge because every cross-module target is reached through a written import. Package naming and build/bridge configuration are validated independently and remain diagnostic rather than key inputs. Cyclic dependency reachability is computed independently and iteratively per root: composing all module fingerprints costs `O(V · (V + E) · log V)` time with `O(V)` scratch, trading repeated bounded traversal for no SCC/index state whose performance value has not been measured. Globally or package-locally disabled caches bypass this fingerprint, closure, and key work. Publication validates both the exact key and the encoded-body digest before accepting a concurrent winner. Ordinary harness-owned paths share one invocation-scoped root only among packages that already enable caching. A custom `run.sh` receives a root inside its per-case, per-implementation scratch directory unless a harness-owned manifest binds its exact relative path and Git object ID into a separately reviewed, snapshotted custom cohort; that cohort gets its own run-scoped root, while unlisted or stale scripts cannot inherit it | `kio-rs/src/cache/typed.rs`, `kio-rs/src/cmd/check.rs`, `ci/run-tests.sh`, `ci/checks/orchestrators/custom-typed-cache/exec-dyn-load-goldens.tsv`, `kio-rs/tests/package_check_cache_skip.rs` |
| Structural type interner, package-version user-elaborator memos, and per-analysis artifact memo | Structural semantic nodes may be interned across package versions. Each handle is 32 bytes on 64-bit targets and retains an optional `Arc` exact concrete view when its source metadata, path-segment spans, forall presentation, ABI/capabilities, phase extension, or descendants differ from the shared node; the handle also carries an optional occurrence-local `Arc` requirement site containing its declaring file and span. Exact-presentation reuse compares that site as well as the concrete view; semantic pointer equality, hashing, and memo keys remain node-based. Populated handle clones share the site, while a new header or projected site allocates its source record and owned path. These sites are transient diagnostic data, not shared semantic-node or serialized phase authority. A polymorphic-instantiation memo hit with the same key representation remains zero-recompute. A semantic entry with a distinct concrete key view is instead an honest presentation miss: substitution and interning derive the current call's concrete result because the cached result carries the first call's recursive source presentation, but only the single semantic entry remains stored. This bounded recomputation avoids both leaking the first writer's presentation and an unbounded per-presentation cache without making persistent keys span- or name-sensitive. In the earlier exact-view-only measurement, which does not measure the occurrence-local requirement carrier, four counterbalanced no-cache checks of the source-heavy `dyn_load_prime` POC per arm showed that the exact-view representation changed median wall from 36.955 to 37.300 seconds (+0.9%), summed user+system CPU from 110.980 to 112.700 seconds (+1.5%), and peak RSS from 828,246 to 858,318 KiB (+3.6%, 29.4 MiB); all outputs were empty and every command exited zero. Content-dependent template, prepared-value, evaluation, and polymorphic-instantiation memos live in an opaque package-version revision; the LSP revision store replaces it when the package fingerprint changes, reuses it when a disappeared focus target falls back to full analysis, and never exposes the reusable memo handle separately from a bound analysis scope. Each fresh `PackageTypecheckScope` is bound to the concrete package or auxiliary-module context it may inspect. The scheduled frontend installs every module that can be forced in that run before constructing the scope, so the lowered package remains one immutable snapshot throughout typechecking. The scope's artifact singleflight pairs one validated evaluator artifact with its exact staged implementation per provider module, elaborator name, and capture-import vector. Eager result inference and deferred module batches share the artifact within that analysis, but no artifact survives into another analysis. Artifact staging rechecks the provider declaration and reachable helper closure into fresh elaborations; staging and validation use private sequential entry points because they can run inside a singleflight winner, where waiting Rayon workers could otherwise starve nested work | `kio-rs/src/pass/typecheck_core/{intern,memo}.rs`, `kio-rs/src/pass/typecheck_full.rs`, `kio-rs/src/cmd/check.rs` |
| Imported newtype-variance memo | One strict-positivity request derives an exact imported provider module's newtype variance table once, retains its ready result in the request-local `VarianceEnv`, and reuses it at every branching occurrence. The resolved package's import graph and exact written provider path determine the lookup; an in-progress entry remains a conservative fail-closed recursion guard. No result crosses the request, module analysis, or phase boundary | `kio-rs/src/pass/typecheck_core/variance.rs`; deterministic firing and avoided-derivation evidence is registered in `audit-compiler-performance` § 8 |
| Exact nominal-scope/provider acceleration and persistent alias contexts | Repeated nominal-head resolution and transparent-alias materialization. Each validated `TopLevelScope` optionally owns one `Arc`-shared index of its written type-shaped selective and qualified type-import edges; modules without such edges retain no index allocation. One call-scoped `NominalProvider` lazily caches exact owner handles, written-edge terminals, and negative borrowed-target lookups without granting spelling-based authority. Alias traversal extends structurally shared exact binder/alpha/frontier maps instead of cloning cumulative scopes, memoizes the free-rigid-name summary of each exact selected virtual view, and threads the incremental alpha allocator across nested alias owners. Phase transformations rebuild the scope from the transformed module so no stale acceleration state crosses a phase boundary. Debug/serde/cache fingerprints continue to derive from the authoritative module/import graph, never the acceleration state | `kio-rs/src/pass/resolve.rs`, `kio-rs/src/pass/typecheck_core/{aliases,persistent_exact}.rs`, `kio-rs/src/cmd/check.rs`, `kio-rs/src/pass/substitute/mod.rs`, `kio-rs/src/prime/typer.rs` |
| Shared annotation/header plans and persistent lexical views | Lambda headers and partial local annotations are classified once before mutation, then the same plan is consumed by direct, retained, and Prime policies. A stack-consumed closed plan borrows the active lexical slice and creates no transport, goal owner, subscriber, or heap state. A plan that must move promotes one exact ambient prefix into the structurally shared `PersistentExactNameMap`; sibling plans clone that root in O(1) and extend only their narrow lexical child. Sparse source-occurrence transport is allocated only when transparent-alias materialization must relate a written `_` to emitted occurrences. This state is transient and never enters serialized, cached, or public phase artifacts | `kio-rs/src/pass/typecheck_core/{annotation_plan,persistent_exact,kind_scheme,apply}.rs`, `kio-rs/src/prime/typer.rs` |
| Empty-hole completed-argument equivalence | Final application completion uses ordinary identity-aware equivalence when no private application-hole parameters or substitutions remain. Successful comparisons avoid the generic hole solver; mismatches fall through to that solver to preserve its structural diagnostic descent, and every nonempty hole state retains the solver unchanged | `kio-rs/src/pass/typecheck_core/apply.rs`, `kio-rs/src/prime/typer.rs`; firing, no-op, diagnostics, and measured-win evidence are registered in `audit-compiler-performance` § 8 |
| Empty-substitution semantic-call handle reuse | Semantic-call selection shares the immutable interned solver, written-diagnostic, and result handles when the substitution is empty and the computed identity-canonical bit exactly equals that handle's existing bit. Every nonempty substitution and every provenance downgrade retains reconstruction, so sharing grants no new identity or phase authority | `kio-rs/src/pass/typecheck_core/apply.rs`; the four provenance cells, nonempty no-op boundary, genuine unfixed RED, and matched measured win are registered in `audit-compiler-performance` § 8 |
| Goal-free retained-publication validation proof | A Lowered owner close performs the complete publication validation once, then may carry the exact canonical goal-free `ScopedType`, rigid scope, store/context/module capability, root kind, and next direct-owner destination in a private move-only typestate. A later direct-parent/root close preserves owner/delta/revision preparation, exact scope extension, source order, and atomic commit while skipping the already-completed scoped-goal-chain, goal-usability, canonicalization, zonking, function-scheme-shape, kind, and publication-escape walks. Pending, noncanonical, and goal-bearing values retain the complete path until those checks first produce the proof; no proof enters Prime, serialization, or a cache | `kio-rs/src/pass/typecheck_core/goals.rs`, `kio-rs/src/pass/typecheck_full/publication.rs`; causal, no-op, adversarial, and measured-win evidence are registered in `audit-compiler-performance` § 8 |
| Deferred-tail classifier and runtime-adapter arenas | Candidate-bearing marked-argument recipe classification and lifted-callback materialization. The classifier's copyable symbol handles refer to invocation-local pair nodes; each pair is marked at most once per semantic role, and lexical binders restore the prior mapping without cloning the live alias environment. The materializer independently separates an optional rebuilt checked subtree from a copyable public/selected/product result handle, allocates one pair node per changed pair occurrence, and rebuilds opaque ancestors whose descendants were retyped. Both arenas, role marks, aliases, and rebuilt-term state are transient and never enter cached, serialized, or public phase artifacts | `kio-rs/src/pass/typecheck_core/apply/fills.rs`, `kio-rs/src/normalization.rs` |
| Equiv-discharge cache and shared streaming key prelude | Warm `kio test` equiv normalization reuse; the body-free evaluator package structure (package file, module paths and imports, non-function declarations, and function headers), primitive environment, and newtype inputs are sanitized and streamed once into a fixed-size digest before the parallel fan-out; each per-`equiv` key adds its substituted terms and the owner-aware transitive closure of reachable function bodies. Reload validates the exact key and encoded-result digest; a following store atomically repairs an invalid entry | `kio-rs/src/cache/equiv.rs`, `kio-rs/src/cmd/test.rs` |
| Operation-owned evaluator driver and iterative final-owner release | Driver-controlled expressions, applications, reflection callbacks and comparison probes retain execution depth in heap-backed continuations. Inline/spilled argument storage, lazy active-call records and borrowed capture ranges are operation-local. Shared evaluator payloads transfer their recursive children into an iterative drain at final release, including escaping values; local checked-term worklists cover reification, result validation, generated-name canonicalization and template replay. Type reconstruction, replay ABI canonicalization, scope-aware type requalification/phase conversion and packet copying are iterative, and transient reconstruction results have single-owner iterative release. Reflected types charge their type and kind structure against the existing exact-call memo-key budget before key construction. This is correctness-primary stack-safety machinery: call records, worklists, payload ownership and retained buffer capacity remain costs in complete-path performance measurements | `kio-rs/src/normalization/{execution,ownership,checked_traversal,replay,type_traversal}.rs`, `kio-rs/src/normalization/execution/` |
| Compile-time evaluator memo caches | Repeated `equiv` / compile-time reduction within a run — fn-closure, eval-function, type-view, and exact-call memoization, admitted only for `memo_candidate` functions | `kio-rs/src/normalization.rs`, `kio-rs/src/pass/typecheck_core/memo.rs` (cache wiring and `ptr_eq` reuse tests); the `equiv` golden corpus is the firing coverage |
| Indexed active structural-recursion sites | Nested compile-time helper re-entry keeps the global per-thread LIFO frame order while an exact structural-site → latest-frame index selects the nearest same-site bound in expected O(1) time. One checked backward link per repeated frame restores the prior mapping on unwind; the transient map retains one entry and one key `Arc` per distinct live site and never enters a phase artifact, cache key, or public API | `kio-rs/src/normalization.rs`; deterministic work, identity, interleave, failure, unwind, layout, and scan-restoration mutation coverage is registered in `audit-compiler-performance` § 8 |
| Linear recursive-signature context partition | Signature replay collects exact-owner type-reference edges once per live canonical epoch with hash-indexed binder shadowing, preserves duplicate written edges for the iterative SCC walk, restores canonical member/component order with one node scan, and compares each retained context with its SCC through one ordered merge. Contiguous canonical module runs, context hashes, and a member bitmap avoid per-declaration or per-context tree work. All indexes and counters are invocation-local or test-only and enter no signature artifact | `kio-rs/src/pass/resolve.rs`, `kio-rs/src/sig/validate.rs`; production-firing, exact-context no-op, graph-work counts, and the avoided nested scan are registered in `audit-compiler-performance` § 8 |
| Indexed fresh-signature origin slices | Signature replay indexes candidate module existence once, constructs each origin's name-only package from referenced heads and import-collision peers, and reuses that validated package for identity qualification. Final epoch construction records each module's first present compile-time import during its declaration pass, before recursive-context deduplication. Independent bounded-size declarations with bounded import lists build one bounded slice each and require one traversal per final epoch construction across modules; ordered indexes retain logarithmic factors. Error diagnostics use the complete name-only origin view, and final semantic validation still checks the complete canonical epoch before and after removals. Indexes and packages are invocation-local; construction and traversal counters are test-only | `kio-rs/src/sig/{validate,replay}.rs`; construction-seam firing, independent restoration of both whole-epoch paths and the per-module scan, slice/full equivalence, and measured scaling are registered in `audit-compiler-performance` § 8 |
| Package-scoped signature identity index | One complete package-signature check shares an existing exact package-owned typecheck scope across its module checks, so the scope's lazy identity-alias index is built once. The scope is fresh for each invocation; individual-module checks keep fresh scopes, and package/module pointer assertions and execution policy remain unchanged. This is the same invocation-local ownership mechanism used by ordinary package typechecking, with no cross-package or persistent reuse | `kio-rs/src/pass/typecheck_core/modules.rs`; actual index-item/build counts, the restored per-module-scope causal, equivalence controls and matched measurements are registered in `audit-compiler-performance` § 8 |
| Direct recursive-component classification | Label expansion, projected declaration emission, split fixes, and recursive-group diagnostics classify each existing SCC from its size or singleton self-edge instead of rescanning the complete cyclic-component list. Classification examines at most the existing components and graph edges, retains projected owner self-edges, and adds no index or phase state | `kio-rs/src/pass/resolve.rs`, `kio-rs/src/pass/resolve/type_rec_classification_tests.rs`, `kio-rs/src/pass/label_elab/mod.rs`; actual-query, examined-edge, complete-output, mixed/projection, scan-restoration, and measured evidence is registered in `audit-compiler-performance` §§ 7–8 |
| Ordered recursive-group recovery correspondence | `recover_to_low` pairs each source recursive-type member with its phase-rebranded member through the converter's one-for-one order rather than rescanning the source group by name for every newtype. Cardinality, variant, name, and binder equality are checked as phase contracts before declaration-owned binders protect payload alias unfolding. The pass retains no index or cache state | `kio-rs/src/pass/recover_to_low.rs`; collision-sensitive wide-group firing, mutation, and measured evidence is registered in `audit-compiler-performance` § 8 |
| Enriched-IR cache | `kio build` structural recovery + optimization reuse. Newtype inputs retain each consumer's written local, selective, or qualified lexical head plus the exact canonical owner/declaration identity; same-provider aliases share inverse-pair semantics without admitting unrelated same-spelled declarations. Reload validates the exact key and encoded-module digest; a following store atomically repairs an invalid entry | `kio-rs/src/cache/enriched.rs`, `kio-rs/src/pass/resolve.rs`, `kio-rs/src/pass/structural_recovery.rs`, `kio-rs/src/pass/optimize.rs`, `kio-rs/src/cmd/build.rs` |
| Emit and artifact caches | `kio build` backend output reuse. Reload validates every exact semantic header/key field and the emitted-text or deterministic content-tree digest before accepting an entry. A following store atomically repairs an invalid entry, and publication accepts only an exact validated concurrent winner. Disabled caches take the ordinary uncached emit path without artifact fingerprint rendering or emit-key context rendering / serialization; semantic target validation, signature handling, and backend preparation still run | `kio-rs/src/cache/emit.rs`, `kio-rs/src/cache/artifact.rs`, `kio-rs/src/cmd/build.rs`, `kio-rs/src/backends/` |
| Bounded Python typed-stub packages | Python stub rendering partitions the one prepared declaration inventory into deterministic private modules of at most 256 declarations, qualifies cross-shard references to their one defining module, and re-exports every shard through one public `__init__.pyi`; it does not duplicate declarations or repeat semantic preparation. This is correctness-primary host-checker scaling rather than a latency optimization. The natural `dyn_load_prime` witness emits 139 shards plus the entry point: 26,972,007 bytes, 111,028 lines, and 35,571 declarations total, with maxima of 256 declarations, 317 top-level statements, 763,016 bytes, and 1,719 lines in one file. Pyright 1.1.411 in strict mode completes that package with no diagnostics in 37.48 seconds wall at 2,996,284 KiB peak RSS (52.09 seconds user, 3.67 seconds system, 148% CPU, zero swaps). The narrow `typed_stub_skin` control emits one shard plus the entry point, 9,164 bytes, 112 lines, and 31 declarations; the equivalent strict package check completes in 0.58 seconds at 135,320 KiB. A matched logical-flat reconstruction of the wide declarations is 23,256,903 bytes and 109,904 lines and stops at Pyright's module-complexity diagnostic after 32.61 seconds at 2,254,256 KiB, so that control establishes the need for bounded modules but makes no latency or RSS improvement claim because it exits early | `kio-rs/src/backends/python/stub.rs`, `kio-rs/src/cmd/build.rs`, `ci/checks/per-case/pyright-strict.sh`, `ci/checks/repo-lint/pyright-strict-selftest.sh`; deterministic structure, cross-shard identity, strict Pyright/mypy execution, runtime/type-view coexistence, stale-artifact replacement, and missing-shard/re-export guards are focused there |
| Exact Python TypedDict definition sharing | Class-form structural dictionaries retain every prepared-site name, but equal ordered field annotations and equal selected TypeVar identities/order share one definition through ordinary aliases. One render-local ordered map retains only exact keys and representative names; functional TypedDicts, nominal carriers, protocols, and recursive expansion keep their existing paths. On the `dyn_load_prime` POC, sharing replaces 28,965 duplicate bodies while retaining all 48,681 declarations and the byte-identical runtime. Stub bytes increase from 36,941,639 to 38,368,290; this saves checker definitions, not source size. With Node 24.18.0, Pyright 1.1.411, strict Python 3.10 checking, and `--max-old-space-size=2048`, the unshared package exhausts the heap after 71.94 seconds at 2,236,824 KiB RSS; shared checks finish without diagnostics in 51.13/49.95 seconds at 2,150,880/2,140,216 KiB. A matched no-cache build pair measures backend emission at 6,068.662/6,132.383 ms and whole-build peak RSS at 1,329,408/1,380,812 KiB; the extra map is a cost, and these measurements establish checker completion rather than compiler acceleration | `kio-rs/src/backends/python/stub.rs` exact-key/no-op unit, `test-data/emissions/python/typed_dict_definition_sharing` portable definition-count proxy, `test-data/emissions/python/typed_stub_skin` strict host interface, and the exact Python `dyn_load_prime` POC |
| Kiodoc snippet cache | Warm `kio doc` snippet re-check, with exact-key and result-body digest validation on reload; a following store atomically repairs an invalid entry | `kio-rs/src/kiodoc/cache.rs`, `kio-rs/src/kiodoc/mod.rs` |
| Semantic cache GC | Bounded on-disk semantic-cache growth | `kio-rs/src/cache/gc.rs`, `kio-rs/src/cmd/cache.rs` |
| LSP open-document caches and bounded stored analysis | Syntax requests and hover/definition/references/completion data. A focused shard and its freshness watermark exist only while its document is open; closing the document drops both. Publishing a successful full analysis advances the watermark for each focused lifecycle whose file appears in that analysis and whose current open-document version the full snapshot covers, preventing older in-flight focused results from resurrecting state. It drops only those focused shards whose snapshots are no newer than that full snapshot. Failed full analyses do not evict shards, and newer, version-uncovered, or unrelated shards remain available. Completion additionally authenticates relevant dependency freshness and the snapshot's captured canonical-path/source identity before reusing typed metadata; a stale dependency schedules focused background refresh, while unrelated overlay edits do not invalidate completion metadata. Authenticated expression completion builds its declaration/provider indexes once per request and reuses them across candidates; this adds no persistent resolver or analysis cache | `kio-rs/src/lsp/state.rs`, `kio-rs/src/lsp/snapshot.rs`, `kio-rs/src/lsp/mod.rs`, `kio-rs/src/lsp/completion.rs` |
| LSP debounce, focused work, cancellation, and worker threads | Interactive diagnostics and typed foreground requests | `kio-rs/src/lsp/worker.rs`, `kio-rs/src/lsp/cancel.rs`, `kio-rs/src/cmd/check.rs` |
| Timing, cache, and sampled-implementation replay probes | Diagnosing frontend, build, cache, LSP, and broad-gate regressions. The debug-only `SAMPLE_IMPL` seed holds each corpus/phase/case implementation assignment constant across a matched comparison and prints the complete mapping; ordinary runs retain fresh random assignments | `kio-rs/src/timing.rs`, `kio-rs/src/cmd/build_timing.rs`, `kio-rs/src/cmd/check.rs`, `kio-rs/src/lsp/worker.rs`, `kio-rs/src/cache/`, `ci/all.sh`, `ci/run-tests.sh` |
| Native CI resource scheduler | One Git-common Rust engine owns `work`, optional capacity-one `cargo`, and `compiler` admission in canonical order. Work defaults to `std::thread::available_parallelism`; omitted compiler capacity uses paced best-effort aggregate feedback, bounded by live CPU and fixed ceilings. One small transient record is read and updated by the FIFO head under the existing state lock; fixed-only activity bypasses it. Idle, unavailable, stale, malformed, and unwritable paths preserve a one-producer fallback. Feedback is not a per-command reservation or OOM guarantee. The versioned compiler trace renders typed decisions outside the state lock and classifies no tool. Unix inherited descriptors, Windows suspended spawn/Job Objects, generic isolated readiness, and keyed bootstrap retain complete-tree lifetime and shared shell/native authority | `ci/schedule.sh`, `ci/cargo.sh`, `ci/infra/kio-ci-scheduler-rs/src/{compiler_admission,compiler_feedback,compiler_trace}.rs`, `ci/infra/kio-ci-scheduler-rs/src/compiler_feedback/host.rs`, native runners; policy/cadence/fallback, current-client minima, queue/trace, lifetime, readiness and platform controls are swept by `audit-compiler-performance` §§ 7–8 |
| Bounded golden worklist and package preflight | One corpus walk inventories case markers and package-discovery inputs. Bounded joins select cases and implementations, validate the standard runner's exact single-package shape once before dispatch, and carry its package identity into each `(case, binary)` unit without per-(case, implementation) package probes | `ci/run-tests.sh`, `ci/checks/repo-lint/run-tests-predispatch-selftest.sh`; causal and measured evidence is registered in `audit-compiler-performance` §§ 7–8 |
| Streaming and reusable dynamic-loader declaration scopes | The Kio-authored dynamic Prime loader streams its unprocessed suffix and reversed processed prefix, then reuses the exact scope advanced past one successfully resolved declaration only for a later declaration with the same owner and a strictly increasing cutoff. Owner/cutoff discontinuities and intervening source-local host functions rebuild the complete scope | `test-data/poc/dyn_load_prime/workdir/loader.kio`, `test-data/poc/dyn_load_prime/run.sh`; firing, fallback, and measured evidence is registered in `audit-compiler-performance` § 8 |
| Exact public bridge scope for scripted dynamic-load hosts | Every `exec_dyn_load_*/run.sh` success golden that vendors the canonical dynamic loader and invokes the fixed `testapi-dyn-load` protocol builds its complete internal loader/interpreter dependency closure but exposes only the eight `testapi` modules used by that host protocol. The cohort lint derives those custom hosts structurally, validates their source and protocol fail-closed, and rejects root-manifest or bridge-surface drift; internal modules remain ordinary imports and continue through typechecking and emission, while the runners that invoke `kio test` retain their package `equiv` checks | `ci/checks/repo-lint/dyn-load-host-surface{,-selftest}.sh`, `ci/infra/kio-test-runner-rs/src/shared/protocol.rs`, `test-data/goldens/00_success/exec_dyn_load_*/{run.sh,workdir/*.pkg.kio}`, `ci/checks/orchestrators/custom-typed-cache/exec-dyn-load-goldens.tsv`; causal and measured evidence is registered in `audit-compiler-performance` § 8 |
| Native test-runner compiler observer | `KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER` supplies one opaque executable outside every actual Rust, Go, Java, Haskell, and Swift runner compile. The shared constructor builds the complete observer / optional Rust cache-wrapper / compiler argv before admission. It is independent of artifact-cache mode and absent from artifact identity; identity probes, runtime launches, warm hits, and same-key waiters stay outside it. The sccache adapter peels only an exact configured observer layer before readiness classification, while the scheduler remains tool-agnostic | `ci/infra/kio-test-runner-rs/src/shared/compiler_observer.rs`, native runner/cache adapters, `ci/infra/sccache.sh`, `ci/checks/repo-lint/schedule-entry-selftest.sh`; parser/argv, per-route construction, miss/hit, readiness, lease, and crash guards are swept by `audit-compiler-performance` § 7 |
| Persistent corpus-tool Cargo targets | Exactly the golden, POC, castle, and contrib orchestrators reuse one worktree-local dedicated target per tool below its owning Cargo workspace across runs. Cargo fingerprints are the sole staleness authority. The generic capacity-one `cargo` lease spans build plus private copy; `ci/cargo.sh` adds `compiler` in canonical order. An explicit scheduler bypass uses an invocation-private target, and workers never execute a Cargo-owned path | `ci/checks/orchestrators/lib/common.sh`, the four corpus-orchestrator call sites, and `ci/checks/repo-lint/schedule-entry-selftest.sh`; firing and optimization justification are registered in `audit-compiler-performance` §§ 7–8 |
| Golden Kio' roundtrip multi-target batching | Expected-success roundtrip rows request the direct target and Kio' target in one producer compiler transaction; a failed combined request replays the two legacy builds fail-closed, preserving target-inapplicability skips and surfacing combined-build regressions | `ci/checks/per-case/kio-prime-roundtrip.sh`, `ci/checks/repo-lint/kio-prime-roundtrip-batching-selftest.sh` |

## No `allow(dead_code)` in hand-written code

Hand-written Rust under `kio-rs/` and `test-data/` must not carry `#![allow(dead_code)]` or `#[allow(dead_code)]`. The attribute suppresses a signal that AGENTS.md § Universal rules — No partial implementations commits to listening to: unused code is scaffolding for work that hasn't landed.

The `kio-rs` library crate root sets the Rust `dead_code` lint to `forbid`, so a
local allowance cannot override this rule anywhere in the compiler modules.
The two thin binary crate roots set the same forbid outside `cfg(test)`: Rust's
synthetic binary test harness injects its own `allow(dead_code)` for the unused
`main`, and those entry points contain no test-only hand-written code. The
repository-wide rule still governs hand-written fixture code outside that
crate. When the compiler flags a symbol as dead, the resolution is one of:

- **Delete the symbol** — the comment claiming "future use" is not a substitute for an actual user. Forward-compat hooks land when the user lands, not before.
- **`#[cfg(test)]`-gate it** — when the symbol is genuinely used only by `#[cfg(test)]` code paths.
- **`#[cfg(feature = "…")]`-gate it** — when a cargo feature genuinely splits which call sites compile.

The single carve-out is `allow(dead_code)` that the Rust backend stamps into *emitted* package code (in `kio-rs/src/backends/rust/emit.rs` and `kio-rs/src/backends/rust/runtime.rs`). An emitted package may not exercise every helper its runtime provides, so the emitted-side allow is load-bearing. These occur as string literals or `push_str` arguments, not as attributes on hand-written items; the distinction is mechanical.

[`audit-partial-implementations`](../skills/audit-partial-implementations/SKILL.md) sweeps for violations.

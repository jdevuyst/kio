# Defining elaborators

An elaborator is ordinary Kio code that runs while a bang call is being
typechecked and returns a checked Kio' term to substitute at that call site.
The declaration defines the source-level call shape; an implementation
function programs against reflected types and already-checked arguments.

This tutorial builds a small identity elaborator, then expands the pieces an
elaborator author needs for structural code generation.

## A complete checked elaborator

The implementation below receives the source type, checked source term, and
an optional target type. It returns the source term unchanged when the caller
provides enough context to solve the target:

<!--kio {file}
package guide;

bridge {
  guide/**;
  library/**;
}
-->

<!--kio {file}
module library;
-->

```kio {file}
module library/identity;

import __intrinsics__;

import __comptime__;

type Target_request = __Type__ | .;

pure fn checked_impl(
  , ct: __Comptime__
  , _source: __Type__
  , value: __Checked_term__
  , target: Target_request
  ) -> __Checked_term__ {
  __either__(
    , __Type__
    , .
    , __Checked_term__
    , target
    , .(_target: __Type__) -> __Checked_term__ { value }
    , .() -> __Checked_term__ { __elab_error__(ct, "identity! needs a target type") }
    )
}

pub elab identity : [Source] Source -> [Target] Target { impl checked_impl }

pub elab identity_block : [Source] Source -> [Target] Target {
  trailing product;
  impl checked_impl
}
```

The consumer is a separate module, so the call imports the elaborator and adds
the bang only at the use site:

<!--kio {harness=identity_consumer file placeholder="__INSERT_CODE_HERE__"}
module guide/main;

import library/identity(identity, identity_block);

__INSERT_CODE_HERE__
-->

```kio {@identity_consumer}
pure fn keep_unit(value: .) -> . { identity!(value, .) }
```

For a generic consumer, the same import and trailing target slot look like:

```kio {ignore}
import library/identity(identity);

fn keep[A](value: A) -> A { identity!(value, A) }
```

The `import` names `identity`, not `identity!`. The bang selects compile-time
elaboration; a name not explicitly imported into the caller's scope does not
resolve.

## Accepting trailing blocks

The second declaration, `identity_block`, uses the same implementation but
accepts its value through a trailing block:

```kio {@identity_consumer}
pure fn keep_block(value: .) -> . {
  identity_block! {
    value
  }
}
```

`trailing product` collects the block's expressions into one ordinary value:
an empty block supplies unit, one entry supplies that entry unchanged, and
several entries supply a product. The implementation receives the checked
value, not a syntax tree. For example:

```kio {@identity_consumer}
pure fn keep_pair(left: ., right: .) -> . & . {
  identity_block! {
    left;
    right
  }
}
```

Semicolons separate entries. A final semicolon does not discard the last
expression or change the result. Product blocks admit expressions rather than
local bindings. A product already stored in `clauses` remains one entry in
`match! value { clauses }`; the product's components are not wrapped in another
product layer.

There are three ways for a declaration to expose a block:

- `trailing product` supplies the expression value or product described above.
- `trailing thunk` supplies an ordinary zero-argument function. Its local
  bindings and final expression form the function body; an empty block returns
  unit. The elaborator decides where that function is called.
- `trailing sequence` supplies an ordinary polymorphic sequencing function.
  It receives a supplied step function and uses it for each `let x <- action;`
  binding or non-final action. Pure `let` bindings remain local, and the block
  ends with an explicit final action.

Descriptors belong to the end of the elaborator's public value arguments, in
the written descriptor order. The first is unlabelled; later descriptors have
distinct labels, such as `trailing thunk else`. Other declaration entries can
appear between descriptors without changing that order. A block call must
supply every required block and cannot leave an ordinary argument group
unapplied. An elaborator with trailing descriptors is called with blocks, not
with blockless parentheses or UFCS.

### Calling a block elaborator

Every block-call head has an adjacent bang. Continuation labels do not:
`if! condition { yes() } else { no() }`. These are imported library names, not
reserved control words. Renaming an elaborator does not change its syntax.

Ordinary values before the blocks are prefix arguments. One unambiguous value
can be bare (`match! value { clauses }`); several use parentheses
(`form!(left, right) { body() }`). One product needs its own parentheses inside
the prefix list: `form!((left, right)) { body() }`. `form!() { body() }` passes
one unit value, whereas `form! { body() }` has no prefix value. Block calls
infer all type arguments; their explicit prefixes contain only values.

Grouping makes nested ownership clear. In `outer! convert!(value) { body() }`,
`convert!(value)` is a blockless prefix argument and the brace belongs to
`outer`. To supply another complete block call as the prefix, write
`outer!(inner! value { first() }) { second() }`. The same boundary is needed
inside an operator operand: `outer! left + (inner! value { first() }) { second() }`.
Greedy operator slots still obey this boundary; an operator's written closing
token can delimit the nested expression instead.

Only a final labelled block can omit its wrapper braces around one complete
nested block call. Thus `if! a { first() } else if! b { second() } else { last() }`
has the same ownership as a braced nested call. If another block belongs to
the outer call, keep the wrapper braces. The formatter performs this elision
only when it preserves the comments.

For a sequence binding, a complete pattern annotation such as
`let .(value: T) <- action;` can check the source payload against `T`. Without
a complete annotation, the source must determine its payload before later
uses of `value`; those later uses cannot rescue an ambiguous source. A partial
annotation on a tuple leaf does not supply an expected type for the whole
source. These are ordinary source-closing binding rules, not a search through
possible providers.

The parser and formatter use the written head, grouping and braces without
loading the elaborator's provider. Once the declaration resolves through an
explicit import, checking validates its descriptors and exposes the blocks
as the ordinary values above. Adding unrelated declarations cannot change
either the parse or the selected elaborator.

## Declaration ABI

The ordinary `impl` field contains a lexical path to a function whose type must
match the reflected ABI below. Use a bare local or selectively imported name
such as `checked_impl`, or select through an explicit module alias such as
`helpers.checked_impl`. The shared path grammar can also name a local/imported
newtype member directly or through an identity alias, but a constructor or
projector has its ordinary member type, which does not match this ABI; define
an ABI-shaped function instead. Calls, inline lambdas, and slash-qualified
package paths such as
`library/helpers.checked_impl` are rejected in the field.

The path follows ordinary expression source order: a same-module ordinary
function, host function, or recursive-group member must appear first. The same
is true of the newtype or identity-alias head used to qualify a constructor or
projector. Local captured types and functions, including host declarations,
must also appear before the elaborator. A same-module alias
or selective self-import does not make a later declaration available early.

This is the default and remains the right choice for an implementation that
needs completed arguments. Every declaration listed in `captures` must be at
least as visible as the elaborator because a quoted capture may survive in the
generated runtime term. An implementation declared in the elaborator's
defining module may remain private, and any implementation may call private
helpers in its own module. A target declared in another module must be
ordinarily importable into the defining module, but need not meet the
elaborator's outward visibility. Callers import the elaborator, not its
compile-time implementation details.

To target another module, import the function selectively or import its module
under an alias. That written `import` is the dependency; the compiler does not
search other modules for a matching function. Adding an unrelated declaration
therefore cannot change which implementation an existing elaborator uses.

Read the declaration as a sequence of call slots:

```text
elab identity : [Source] Source -> [Target] Target
```

For each bang call, the typer first applies the normal call rules: explicit
type arguments, `_` placeholders, value arguments, and an expected result type
all constrain those slots. The implementation's private parameter types do
not change call completion, grouping, partial application, or UFCS placement.
It then calls the implementation with:

1. a leading `__Comptime__` proof;
2. any declared captures, in capture-list order;
3. one reflected argument for every call slot, in declaration order; and
4. an expected return value of `__Checked_term__`.

A value slot such as `Source` becomes `__Checked_term__`. A type slot becomes
`__Type__` when the implementation requires it to be solved before evaluation,
or `__Type__ | .` when the implementation accepts an unresolved request as
unit. In the example, `Source`, the source value, and trailing `Target` become
`_source`, `value`, and `target` respectively. Optional absence controls only
whether that already-selected action may run: the exact declaration binder
must occur in the result, must have no value dependency, and must not be
shadowed before that result occurrence. It does not turn an unresolved `_`
into a new polymorphic residual.

After evaluation, the typer checks the returned term against the declared
result. If result variables remain unsolved, the returned term's actual type
may solve them. This is target inference: it is local unification against the
declared result shape, not a search through declarations. A returned term with
the wrong type is rejected rather than inserted.

### Marked fills implementations

Use `impl(fills)` when an implementation must consume marked provisional
operands. Usually it relates an open declared result to value operands whose
structural headers are known before their bodies finish typechecking. The
reference `derive!` implementation instead returns the context unchanged and
uses the marked reflected handles so specialization preserves an enclosing
generic binder's identity. Ordinary `impl` declarations are unchanged and do
not receive fill state.

A marked implementation receives:

1. the same leading `__Comptime__` proof;
2. a compiler-created `__Fill_ctx__` immediately after it;
3. captures and reflected declared slots in the same order as ordinary mode;
   and
4. an expected result of `__Checked_term__ & __Fill_ctx__`.

Every marked type slot is `__Type__`. Each value slot is a checked recipe whose
product and lambda headers are available even when a nested body is waiting
for an expected type. The implementation runs once over those headers, returns
one checked result and the final context, and cannot enter or inspect a pending
body itself. It may return the original context unchanged when it has no
relation to append.

Before that implementation runs, every computed input needs a determined type,
even if the implementation will ignore the value. A known generic type such as
`Box(A)` is determined when `A` is a declared parameter; a factory call whose
type argument is still being inferred is not. The outside expected result and
already known sibling arguments may supply that information through the public
call type, but a later fill or callback body cannot supply it afterward.
Explicit tuples apply these rules to their components. For example, if `value`
has a known type and `identity` has type `[A] A -> A`, both calls in
`(identity(value), identity(value))` have determined types. A direct lambda is the
exception: all of its type binders and value parameters must be complete, and
no `_` placeholder may remain. Only unresolved goals in its final body result
may stay open. Any other input with an undetermined type fails at its source
before the implementation runs. An ordinary `let` can finish a call whose
result needs callback-body checking. It cannot invent a missing generic type
argument: supply that argument or a complete binding annotation when neither
the arguments nor the context determine it.

An ordinary call can also obtain its result type from a fully annotated
callback without checking the callback body first. For example, given
`drive[S][R](step: S -> S | R, seed: S) -> R`, the callback signature in
`drive(.(state: State) -> State | Done { advance(state) }, seed)` determines
`R` as `Done`, just as passing a named function of that type does. This works
through enclosing ordinary calls too. The body is still checked before the
elaborated call is accepted, even if the implementation discards its value.
A missing parameter or result annotation cannot be discovered from the body
to make an unfinished producer ready for the elaborator.

A polymorphic lambda literal can omit its return annotation, or write `-> _`,
when its type binders and parameter types are written completely and the result
can be determined without reading its body. For example, a surrounding
`String` result can constrain the result of `.[A](boxed: Box(A)) { label(boxed) }`
through the elaborator's fills. The implementation runs over the provisional
header first; then the compiler checks the one generic body against that
independently determined result. Ignoring a source does not excuse checking
its body, and an undetermined result is rejected before the body is entered.

This provisional result can use types from the surrounding scope, but cannot
depend on the lambda's new `A`. Write a concrete `-> A` annotation, or supply a
complete expected function type, for a result that depends on that binder.
Those existing routes and monomorphic lambda inference are unchanged. An
omitted return on a top-level `fn` still means unit; this rule does not infer
top-level return types or unfinished results of ordinary producer calls.

`__fill__(ct, fills, destination, candidate)` returns a new context with one
relation appended. Thread that returned context explicitly: the order of those
calls is the authored fill order. A destination must be the marked call's
declared result handle, not a reflected input or constructed tuple. Relations
for the same destination are adopted together as one finite common-result
group; append order preserves traversal but does not choose an inference
anchor. If the destination is already known, every candidate checks it. If it
is open, any candidate whose result is independently determined may establish
or refine it, and all other candidates must agree. A conflict or a group with
no determining candidate fails as a non-directional common-result problem. The
returned checked result does not implicitly fill an open marked target. Because
the group is exactly the finite authenticated transcript, adding an ambient
declaration cannot change it.

`__term_specialize__(ct, term, pattern, target)` specializes a provisional
term's explicit leading type binders by exact structural matching, not
subtyping or containment, without changing fill state. Its three
arms are a specialized checked term, unit for a normal structural mismatch,
and diagnostic text for malformed or underdetermined input.

During marked header staging, the resolved canonical product intrinsic exposes
its two value operands because its intrinsic classification and exact
`[A][B](A, B) -> A & B` scheme prove their one-to-one order. The compiler still
completes that original call exactly once. An ordinary function with the same
type remains one opaque value: its body may reorder, duplicate, or discard the
arguments. This is a resolved builtin boundary, not behavior attached to a
user-spellable function name.

## Reflected types and checked terms

`import __comptime__;` introduces the compiler-owned reflection vocabulary:

- `__Comptime__` is the immutable proof supplied first to every implementation.
- `__Fill_ctx__` is the opaque, call-local transcript supplied second only to
  `impl(fills)` implementations; source code cannot construct one.
- `__Type__` is an opaque handle to a resolved Kio type.
- `__Checked_term__` is a Kio' term paired with the type the compiler already
  checked for it.
- `__Type_name__`, `__Type_var__`, and `__Type_arity__` expose the bounded
  handles needed to inspect and rebuild types without turning compiler state
  into ordinary package data.
- `__Diagnostic_text__`, `Comptime_str`, and `Comptime_bool` are compile-time
  message, string, and predicate values.

Every callable helper takes `__Comptime__` first. Only `__fill__` additionally
takes `__Fill_ctx__` second and returns the descendant context.

`__reflect_type__(ct, T)` reflects a solved type argument. `__type_view__`
decomposes a type into the complete structural view: unit, bottom, product,
sum, function, forall, type variable, or named head. Equality helpers preserve
nominal and host identities; transparent aliases unfold.

A checked term is stronger than an untyped syntax tree. Construction helpers
validate the type at every step, so an elaborator cannot return malformed Kio'
and rely on a backend to discover the problem.

## Constructing types and terms

Type constructors such as `__type_product__`, `__type_sum__`,
`__type_arrow__`, and `__type_forall__` build reflected types. Checked-term
constructors such as `__term_fn__`, `__term_call__`, `__term_let__`, and
`__term_pair__` build typed Kio' nodes.

This helper builds the checked identity function at any reflected type:

<!--kio {harness=elaborator_helper placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import __comptime__;

__INSERT_CODE_HERE__
-->

```kio {@elaborator_helper}
pure fn identity_term(ct: __Comptime__, typ: __Type__) -> __Checked_term__ {
  let one = __type_arity_succ__(ct, __type_arity_zero__(ct));
  let fn_type = __type_arrow__(ct, typ, one, typ);
  __term_fn__(ct, fn_type, .(parameter: __Checked_term__) -> __Checked_term__ { parameter })
}
```

`__term_fn__` creates a hygienic parameter and passes its checked reference to
the callback. If `fn_type` is not a one-parameter function with the callback's
result type, construction cannot yield a valid term. `__term_call__` applies
the same discipline to a callee and argument packet.

Use `__term_type__(ct, term)` to inspect a checked argument's exact type.
Prefer the supplied folds over reflected products, sums, arguments, arities,
and function parameters to ad hoc encoding of compiler data.

After aliases unfold, an unresolved outer type or head keeps
`__type_args_fold__` or `__type_apply__` pending. The fold does not assume zero
arguments, and type application does not discard its argument. This prevents
an open reflected handle from producing a concrete answer before its outer
structure is known.

## Three similarly named surfaces

These APIs live at different phases:

- `import __intrinsics__;` introduces runtime Kio values such as `__pair__`,
  `__left__`, `__right__`, `__either__`, and `__fst__`. An elaborator
  implementation may call them while computing, but they are still the core
  values ordinary Kio' terms execute.
- `import __comptime__;` introduces reflection and checked-term construction.
  These helpers run only during checking and do not become package runtime
  calls.
- Names such as `__intrinsic_pair__` and `__intrinsic_either__` are
  `__comptime__` checked-term constructors. They construct a checked term that
  calls the corresponding runtime intrinsic; they are not aliases for calling
  `__pair__` or `__either__` inside the implementation.

The one-to-one spelling is intentional: runtime `__pair__` corresponds to
checked-term constructor `__intrinsic_pair__`. Core term constructors and
intrinsic-call constructors remain separate so generated syntax cannot blur
the phase boundary.

## Captures

An elaborator does not gain arbitrary runtime values from its defining module.
List each declaration whose reflected type or checked runtime term the
implementation needs:

```kio {ignore}
pub elab checked : [Source] Source -> [Target] Target {
  captures (I32, id_i32);
  impl checked_impl
}
```

The capture list prepends implementation parameters in exactly that order. A
captured type contributes a quoted `__Type__`; a captured value contributes a
quoted `__Checked_term__`. Capturing does not execute the declaration during
elaboration, so host types and host functions may be captured for insertion
into generated runtime code. The corresponding ABI starts:

```kio {ignore}
pure fn checked_impl(
  , ct: __Comptime__
  , i32_type: __Type__
  , id_i32_term: __Checked_term__
  , source: __Type__
  , value: __Checked_term__
  , target: __Type__ | .
  ) -> __Checked_term__ {
  // ...
}
```

Captures are explicit dependency injection for generated code. Their visibility
must be at least that of the elaborator because generated code may retain them.
They also keep name resolution open-world: adding another declaration to a
module cannot change an existing elaboration because the implementation's input
set does not grow implicitly.

## Diagnostics

Return `__elab_error__(ct, message)` when the requested elaboration is not
defined for the supplied shape. It reports the elaborator-error category at
the bang call. Return `__type_error__(ct, message)` when the appropriate result
is an ordinary type error. Both are checked-term sentinels, not runtime terms.

Build messages as `__Diagnostic_text__` with the display and concatenation
helpers. Include the operation, the reflected type or shape that failed, and
the constraint the caller can satisfy. Do not panic for a user-selectable input
shape; reserve an internal failure for an invariant that earlier checked-term
construction promised was unreachable.

## Purity

An `elab` declaration has no `pure` modifier. Its implementation always runs in
a pure compile-time context. The implementation must therefore resolve to a
`pure fn`, and every ordinary helper it reaches transitively must also be
marked `pure`.

That restriction applies to executable references in implementation bodies,
not to types or quoted runtime dependencies. Implementation ABI types and
local type annotations may mention any well-formed type, including host types;
named types in the elaborator's call type obey its visibility floor. Captures
remain subject to their declaration-visibility rule. A captured host function
is a checked term the implementation may put into generated runtime code; the
implementation does not call that function merely by capturing it.

Purity of the generated runtime term is a separate question. After elaborator
results replace their bang calls, the compiler validates the assembled Kio'
package again. A generated host-function or unmarked-function reference is
rejected when it lands in a `pure fn`, while the same term is allowed in an
ordinary function. A generated pure term is allowed in either.

## Total compile-time recursion

Use the ordinary helper call `__structural_recur__(...)` for recursive walks.
It receives an initial `fuel`, an `input`, and a step callback. The callback's
first parameter is itself a callback of type `(fuel & input) -> result`,
followed by the current fuel and input. Every path must return the same
`result` type.

The evaluator measures the root fuel and checks every recursive callback:

- the next fuel must have a recursive measure strictly below both the current
  measure and the stored root measure; evaluator-created callbacks maintain
  `current ≤ root`, while both bounds are checked independently for a callback
  constructed directly through the evaluator API;
- each resolved `__structural_recur__` source occurrence supplies a helper
  origin that survives proof/type application stages and higher-order use;
- the helper's three operands and the callback's two operands may be written
  as flattened arguments or as one right-folded product packet;
- after all call operands are evaluated, the first invocation at an origin
  uses the initial fuel measure; re-entering that origin uses the recursive
  measure and must descend strictly from the nearest active invocation;
- invocations from distinct helper origins do not compare measures;
- equal-measure fuel is rejected regardless of its value identity,
  representation, or whether it contains a projected handle;
- unit, literals, finite constructor trees with measurable children,
  structurally determined reflection handles, checked terms, arities, and
  diagnostic text are measurable;
- an initial projected type uses its authenticated alias-unfolded structural
  carrier as a conservative lower bound; its exact scoped snapshot is
  retained for whole-handle reflection and fills, while an unknown outer
  carrier is not measurable and a direct projected value used for recursion
  must be complete;
- when an ordinary finite constructor carries a projected field, a known outer
  structure gives that field the same conservative size in both measurements;
  an unknown outer field still makes the constructor unmeasurable;
- a determined child can remain measurable even when its parent is open;
- closures, opaque atoms, stuck terms, case splits, and recursive callbacks are
  not valid fuel.

Unmeasurable initial or recursive fuel, a callback that does not strictly
descend from its current fuel or stay below its stored root, or an active same
helper origin re-entry that does not strictly descend is a totality failure
(exit code `16`), with root/current/next measures included when available. If
both callback bounds fail, the diagnostic reports the current edge; the
root-specific diagnostic applies only when the current edge descends but the
root bound fails. Once an evaluated edge fails, an enclosing let, sequence,
callback, or ordinary function cannot discard the failure; that compile-time
evaluation reports its first totality failure. This check is what permits
recursive elaborator code without weakening compile-time termination.

The generated [Builtin modules](builtin-modules.md#structural_recur) reference
contains the complete helper scheme and a checked countdown example.

## From implementation to Kio'

Successful evaluation returns one checked term (and, in marked mode, a fill
context consumed by the typer). The typer records the completed term in a
boundary-local elaboration table; substitution swaps it for the bang call at
the Lowered → Prime boundary. The assembled package is then validated as
standalone Kio', including each ordinary function's retained `pure` promise.
Neither the bang syntax, implementation mode, reflected values, fill context,
transcript, executable compile-time implementation, nor hidden typer state
crosses that boundary. Backends receive only ordinary, validated Kio'.

For reusable implementations, read the checked
[`elab` library](using-libraries.md#the-elaborator-library) alongside this
tutorial. The exhaustive helper contracts remain in
[Builtin modules](builtin-modules.md).

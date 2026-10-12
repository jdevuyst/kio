# Building and testing elaborators

This guide develops small elaborators that construct new code, reuse a captured
function, walk a reflected type, and fill a shared result. Each implementation
has a caller and a behavior you can check. It uses existing checked-term
constructors: a checked term is a typed template for generated code, not a
string containing source.

You should already be comfortable with generic functions, products, sums, and
explicit imports from the [language tutorial](../tutorials/language.md).
[Defining elaborators](elaborators.md) explains the declaration ABI and the
detailed reflection, purity, block, and fill rules. Keep the
[builtin reference](builtin-modules.md) nearby for helper signatures.

## Set up a package you can inspect

Create `author.pkg.kio`:

```kio {file}
package author;

bridge {
  author/**;
}

build {
  target kio-prime {
    out "out/kio-prime/"
  }
}
```

Create the root module, `author.kio`:

```kio {file}
module author;
```

The following sections give complete files under `author/`. Calls and laws
live in separate consumer files. You can add them as you reach each section;
the package needs no host implementation for checking or testing these laws.
Run commands from the directory containing `author.pkg.kio`:

```sh
kio check
kio test
kio build kio-prime
```

Checking a function that returns `__Checked_term__` checks the implementation's
ordinary Kio type. Calling its elaborator also executes construction and checks
the resulting generated term. `kio test` then checks the `equiv` claims about
that term. Keep all three steps in your authoring loop.

## Construct a let, a lambda, and a call

Our first transformation turns one value into two copies. Its generated
program binds the source once, constructs a function that repeats its
parameter, and calls that function with the saved source.

Create `author/duplicate.kio`:

```kio {file}
module author/duplicate;

import __comptime__;

pure fn twice_impl(ct: __Comptime__, source: __Type__, value: __Checked_term__) -> __Checked_term__ {
  __term_let__(ct, source, value, .(saved: __Checked_term__) -> __Checked_term__ {
    let one = __type_arity_succ__(ct, __type_arity_zero__(ct));
    let result = __type_product__(ct, source, source);
    let fn_type = __type_arrow__(ct, source, one, result);
    let repeat = __term_fn__(ct, fn_type, .(parameter: __Checked_term__) -> __Checked_term__ {
      __intrinsic_pair__(ct, parameter, parameter)
    });
    __term_call__(ct, fn_type, repeat, saved)
  })
}

pub elab twice : [A] A -> (A & A) { impl twice_impl }

pub elab twice_block : [A] A -> (A & A) { trailing product; impl twice_impl }
```

The declaration has two call slots: the type `A` and its value. Consequently,
the implementation receives `source` and `value` after the compile-time proof.
The result `A & A` needs no separate target slot.

These are two different computations:

| During elaboration | When generated code runs |
| --- | --- |
| `source` is a reflected `__Type__` handle | The source has ordinary type `A` |
| `value`, `saved`, and `parameter` are checked pieces of code | The source expression runs once; its saved value is reused |
| `__term_fn__` runs its construction callback | The constructed lambda runs when called |
| `__intrinsic_pair__` constructs a pair expression | That expression produces a pair of ordinary values |
| `__term_call__` constructs an application | The constructed application calls the lambda |

The callbacks receive fresh checked references to generated binders. The name
`saved` in the implementation does not look up a caller local called `saved`.
Likewise, `parameter` cannot capture a caller's local with that spelling.
This is hygiene: generated bindings keep their own identities.

The arity `one` describes one runtime parameter. Its parameter packet has type
`source`; the result has type `source & source`. A function with two parameters
instead has a product packet and arity two. A function taking one product
parameter still has arity one. Use the actual function type when constructing
calls; the packet's product shape alone does not establish parameter count.

Create `author/duplicate_checks.kio`:

```kio {file}
module author/duplicate_checks;

import author/duplicate(twice, twice_block);

pure fn duplicate[A](saved: A) -> A & A {
  let parameter = saved;
  twice!(parameter)
}

equiv repeats_the_input[A](saved: A) {
  duplicate(A, saved);
  (saved, saved)
}

equiv product_block(left: ., right: .) {
  twice_block! { left; right };
  ((left, right), (left, right))
}
```

This caller deliberately uses the implementation callbacks' names. The law
checks the generated let, lambda, and application, not just an unchanged
input. The block law also invokes `twice_block`: two block entries supply one
product value, which the transformation duplicates.

Do not replace the generated let with
`__intrinsic_pair__(ct, value, value)` when the source can perform effects.
That builds two occurrences of the source expression. Binding it once states
the intended evaluation count independently of what that source does.

## Reuse an ordinary polymorphic function as a template

For a fixed runtime skeleton, write the skeleton in ordinary Kio and capture
its checked reference. Create `author/template.kio`:

```kio {file}
module author/template;

import __comptime__;

pub pure fn repeat[A](value: A) -> A & A { (value, value) }

pub pure fn identity[A](value: A) -> A { value }

pure fn template_impl(
  , ct: __Comptime__
  , repeat_template: __Checked_term__
  , source: __Type__
  , value: __Checked_term__
  ) -> __Checked_term__ {
  let specialized = __term_type_app__(ct, repeat_template, source);
  __term_call__(ct, __term_type__(ct, specialized), specialized, value)
}

pub elab template_twice : [A] A -> (A & A) { captures (repeat); impl template_impl }
```

Captures precede the declared call slots in the implementation ABI.
`repeat_template` denotes the explicitly listed function. It does not execute
`repeat` during elaboration. `__term_type_app__` constructs its specialization
at the reflected source type; `__term_call__` constructs the runtime call.
Ordinary function application evaluates the source argument once.

Both the implementation and `repeat` are `pure fn`s, for different reasons.
The implementation must execute purely while checking. The generated reference
to `repeat` is allowed in a pure caller because `repeat` itself is pure.
Captures must be at least as visible as the elaborator; `pub repeat` can remain
in code generated by `pub template_twice`. The implementation stays private.

Create `author/template_checks.kio`:

```kio {file}
module author/template_checks;

import author/duplicate(twice);
import author/template(template_twice);

pure fn duplicated[A](value: A) -> A & A { template_twice!(value) }

equiv templates_and_constructors_agree[A](value: A) {
  duplicated(A, value);
  twice!(value);
  (value, value)
}
```

The law checks behavior. It does not claim that a retained helper call and a
constructed inline lambda emit identical text or have identical performance.
Choose the form that makes your generator easier to understand.

When the required type argument is already known, as here,
`__term_type_app__` is sufficient. For candidate search,
[`__term_specialize__`](builtin-modules.md) instead matches an
explicit pattern against a target. Its result has three distinct outcomes:
a specialized checked term, unit for an ordinary mismatch, and diagnostic text
for malformed or undetermined input. Use the successful term, continue to
another explicit candidate on a mismatch, and report the diagnostic arm.
None of those outcomes appends a fill relation or searches ambient declarations.

## Walk an actual reflected product tree

Now build `mirror!`, which reverses every binary product while leaving other
types alone. A right-spine fold is enough for a flat tuple operation. Mirroring
also visits products nested on the left, so this implementation uses an
explicit recursive walk.

The reflected type view is positional. In order, its alternatives are unit,
bottom, product, sum, function, forall, type variable, and named head. Unit and
bottom both carry unit; product and sum both carry a pair of type handles.
Matching only their payload types would confuse these alternatives. The helper
below removes the first two alternatives, selects the third, and treats the
remaining five as non-products.

Create `author/mirror.kio`:

```kio {file}
module author/mirror;

import __intrinsics__;

import __comptime__;

type Type_pair = __Type__ & __Type__;

type After_product =
  | Type_pair
  | (__Type__ & __Type_arity__ & __Type__)
  | (__Type_var__ & __Type_arity__ & __Type__)
  | __Type_var__
  | __Type_name__
  ;

type From_product = Type_pair | After_product;

type After_unit = . | From_product;

type Maybe_product = Type_pair | .;

pure fn product_children(ct: __Comptime__, typ: __Type__) -> Maybe_product {
  __either__(
    , .
    , After_unit
    , Maybe_product
    , __type_view__(ct, typ)
    , .(_unit: .) -> Maybe_product { __right__(Type_pair, ., ()) }
    , .(after_unit: After_unit) -> Maybe_product {
        __either__(
          , .
          , From_product
          , Maybe_product
          , after_unit
          , .(_bottom: .) -> Maybe_product { __right__(Type_pair, ., ()) }
          , .(from_product: From_product) -> Maybe_product {
              __either__(
                , Type_pair
                , After_product
                , Maybe_product
                , from_product
                , .(children: Type_pair) -> Maybe_product { __left__(Type_pair, ., children) }
                , .(_other: After_product) -> Maybe_product { __right__(Type_pair, ., ()) }
                )
            }
          )
      }
    )
}

pure fn mirror_term(ct: __Comptime__, source: __Type__, value: __Checked_term__) -> __Checked_term__ {
  __structural_recur__(
    , ct
    , __Type__
    , __Checked_term__
    , __Checked_term__
    , source
    , value
    , .(
        , recur: (__Type__ & __Checked_term__) -> __Checked_term__
        , current_type: __Type__
        , current_term: __Checked_term__
        ) -> __Checked_term__ {
        __either__(
          , Type_pair
          , .
          , __Checked_term__
          , product_children(ct, current_type)
          , .(children: Type_pair) -> __Checked_term__ {
              let left_type = __fst__(__Type__, __Type__, children);
              let right_type = __snd__(__Type__, __Type__, children);
              let left = __intrinsic_fst__(ct, current_type, current_term);
              let right = __intrinsic_snd__(ct, current_type, current_term);
              __intrinsic_pair__(ct, recur(right_type, right), recur(left_type, left))
            }
          , .(_leaf: .) -> __Checked_term__ { current_term }
          )
      }
    )
}

pure fn mirror_impl(
  , ct: __Comptime__
  , source: __Type__
  , value: __Checked_term__
  , _target: __Type__ | .
  ) -> __Checked_term__ {
  __term_let__(ct, source, value, .(saved: __Checked_term__) -> __Checked_term__ {
    mirror_term(ct, source, saved)
  })
}

pub elab mirror : [Source] Source -> [Target] Target { impl mirror_impl }
```

The three explicit helper type arguments are fuel, state, and result.
`current_type` is fuel: each recursive call uses one of its actual reflected
children, which has a strictly smaller structural measure. `current_term` is
state: it describes the corresponding projection, and may grow as the walk
adds projections. Its growth does not decide termination. Both recursive
branches return checked terms, and the pair constructor derives their new
product type.

The initial let keeps projections from duplicating the source computation.
At a non-product the term passes through unchanged, including a sum or an
opaque generic type. Transparent aliases unfold when their structure is
viewed; distinct nominal identities remain distinct.

`_target` accepts either a solved target or unit for an unresolved request.
Returning a checked term lets the ordinary implementation infer `Target` from
that term's type. This is different from the marked fill protocol below.

Create `author/mirror_checks.kio`:

```kio {file}
module author/mirror_checks;

import author/mirror(mirror);

pure fn mirrored[A][B][C](a: A, b: B, c: C) -> C & (B & A) { mirror!(((a, b), c)) }

equiv mirrors_nested_products[A][B][C](a: A, b: B, c: C) {
  mirrored(A, B, C, a, b, c);
  (c, (b, a))
}

equiv keeps_a_sum(value: . | (. & .)) {
  mirror!(value);
  value
}

equiv infers_the_generated_type[A][B](a: A, b: B) {
  .(pair: B & A) { pair }(mirror!((a, b)));
  (b, a)
}
```

The second law matters: a sum containing a product is a leaf for this
particular algorithm. Selecting the product view by payload type would get
that case wrong. For a task that only visits a product's right-hand chain,
prefer [`__type_product_spine_fold__`](builtin-modules.md).
The reference also provides folds for sum spines, function parameters, type
arguments, and arities.

### Check a non-descending recursion edge

This negative example is a separate file, not part of the successful package.
It invokes the implementation and recurses with its unchanged fuel:

```kio {check_exit_code=16}
module bad_recursion;

import __comptime__;

pure fn stuck_impl(ct: __Comptime__, typ: __Type__) -> __Checked_term__ {
  __structural_recur__(
  , ct
  , __Type__
  , .
  , __Checked_term__
  , typ
  , ()
  , .(recur: (__Type__ & .) -> __Checked_term__, fuel: __Type__, _state: .) -> __Checked_term__ {
      recur(fuel, ())
    }
  )
}

elab stuck : [A] . { impl stuck_impl }

fn trigger() { stuck!(.) }
```

`kio check` reports totality error `16`. The next fuel must shrink on every
recursive edge; a different state or freshly rebuilt equal-size fuel does not
make the edge valid. See [total compile-time recursion](elaborators.md#total-compile-time-recursion)
for the complete measurement and callback rules.

## Fill a result before checking pending bodies

An ordinary implementation receives completed operands. Use `impl(fills)`
when a known operand header must help determine a result needed by another
operand's body. The implementation receives a call-local `__Fill_ctx__` and
returns both generated code and the final context.

Our `first_result!` takes two zero-argument functions, requires their results
to agree, and constructs a call to the first. Create `author/fills.kio`:

```kio {file}
module author/fills;

import __comptime__;

pure fn invoke(ct: __Comptime__, thunk: __Checked_term__) -> __Checked_term__ {
  __term_call__(ct, __term_type__(ct, thunk), thunk, __term_unit__(ct))
}

pure fn first_impl(
  , ct: __Comptime__
  , fills: __Fill_ctx__
  , result_type: __Type__
  , first: __Checked_term__
  , second: __Checked_term__
  ) -> __Checked_term__ & __Fill_ctx__ {
  let first_result = invoke(ct, first);
  let second_result = invoke(ct, second);
  let with_first = __fill__(ct, fills, result_type, __term_type__(ct, first_result));
  let with_second = __fill__(ct, with_first, result_type, __term_type__(ct, second_result));
  (first_result, with_second)
}

pub elab first_result : [Result] (. -> Result) -> (. -> Result) -> Result { impl(fills) first_impl }
```

`result_type` is the marked call's declared `Result` handle. That is the
destination allowed by `__fill__`; a reflected input or a freshly constructed
type is not a substitute. Each candidate is the result type of one constructed
thunk call. The second append uses `with_first`, and the returned pair carries
`with_second`. Ignoring these returned contexts would lose the relations.

Constructing `second_result` does not run the second thunk. It supplies a
candidate type; only `first_result` enters the generated program. The compiler
still checks both input bodies after adopting the fills, including the body
whose code is discarded.

Create `author/fill_checks.kio`:

```kio {file}
module author/fill_checks;

import author/fills(first_result);
import author/template(identity);

pure fn known_result(value: .) -> . {
  first_result!(.() { value }, .() { value })
}

pure fn open_first(value: .) -> . {
  let selected = first_result!(.() { identity(_) }, .() -> (. -> .) { identity(.) });
  selected(value)
}

pure fn concrete_first(value: .) -> . {
  let selected = first_result!(.() -> (. -> .) { identity(.) }, .() { identity(_) });
  selected(value)
}

equiv fills_agree_in_both_orders(value: .) {
  known_result(value);
  open_first(value);
  concrete_first(value);
  value
}
```

`known_result` supplies an expected unit result. The other calls have an open
local result: the explicitly annotated thunk supplies `. -> .`, which lets
the other body finish `identity(_)`. Reversing the operands retains that
inferred type. Fill order records traversal; it does not make the first
candidate win. The returned checked term alone does not fill an open marked
result.

For the following negative examples, use a fresh consumer module importing
the same two declarations. Keep rejected examples outside the successful
package while you run its laws.

<!--kio {harness=fill_failure file placeholder="__SNIPPET__"}
module author/fill_failure;

import author/fills(first_result);
import author/template(identity);

__SNIPPET__
-->

Two concrete results disagree:

```kio {@fill_failure check_exit_code=14}
fn disagree(value: .) {
  let _selected = first_result!(.() { value }, .() { (value, value) });
  ()
}
```

With no independently determined result, neither pending body supplies an
anchor:

```kio {@fill_failure check_exit_code=14}
fn no_anchor() {
  let _selected = first_result!(.() { identity(_) }, .() { identity(_) });
  ()
}
```

Both fail with type error `14`. Do not reorder operands to repair them.
For `disagree`, change the bodies so their results agree; an annotation can
locate that mismatch but cannot make unit equal a pair. For `no_anchor`, give
the call a concrete expected result, or make a candidate's result independently
known. A marked operand's parameters and type binders must already be complete;
only its final body result may remain pending. The
[fills reference](elaborators.md#marked-fills-implementations) explains computed
inputs, polymorphic headers, and results dependent on a lambda's own binder.

## Implement thunk and sequence blocks

`twice_block!` already exercised a product block. A thunk block supplies a
zero-argument function; a sequence block supplies a function parameterized by
the caller's bind operation. Their implementations construct ordinary calls.

Create `author/blocks.kio`:

```kio {file}
module author/blocks;

import __comptime__;

pure fn scope_impl(ct: __Comptime__, _result: __Type__, body: __Checked_term__) -> __Checked_term__ {
  __term_call__(ct, __term_type__(ct, body), body, __term_unit__(ct))
}

pub elab scope : [Result] (. -> Result) -> Result { trailing thunk; impl scope_impl }

pure fn sequence_impl(
  , ct: __Comptime__
  , fills: __Fill_ctx__
  , _receiver_type: __Type__
  , result_type: __Type__
  , receiver: __Checked_term__
  , body: __Checked_term__
  ) -> __Checked_term__ & __Fill_ctx__ {
  let result = __term_call__(ct, __term_type__(ct, body), body, receiver);
  (result, __fill__(ct, fills, result_type, __term_type__(ct, result)))
}

pub elab sequence : [Receiver][Result] Receiver -> (Receiver -> Result) -> Result {
  trailing sequence;
  impl(fills) sequence_impl
}
```

`scope_impl` invokes the thunk in the generated program. The local bindings
inside its block therefore run when that program runs. `sequence_impl` applies
the sequence function to the explicit receiver and threads its result fill.
Neither implementation evaluates a source block during construction.

Create `author/block_checks.kio`:

```kio {file}
module author/block_checks;

import author/blocks(scope, sequence);

newtype Box[A] : A { constructor box; projector un_box }

pure fn bind[A][B](source: Box(A), next: A -> Box(B)) -> Box(B) {
  next(Box.un_box(source))
}

pure fn scoped[A](value: A) -> A {
  scope! { let saved = value; saved }
}

pure fn sequenced[A](value: A) -> Box(A) {
  sequence! bind {
    let saved <- Box.box(value);
    let copy = saved;
    Box.box(copy)
  }
}

equiv block_execution[A](value: A) {
  scoped(A, value);
  Box.un_box(sequenced(A, value));
  value
}
```

The sequence lowers to a call to `bind` with a continuation. The ordinary
`let copy` stays inside that continuation; the final expression is an action
of type `Box(A)`. This receiver unwraps and continues immediately. Other
receivers define their own sequencing behavior through the same explicit
typed interface. The descriptor does not select a bind implementation from
the package's declarations.

## Test errors at the right boundary

Use negative consumers to distinguish an unsupported input, a generated type
error, and a purity error. Each example below includes a consumer call.

### Report a user-selectable shape

This standalone module accepts only unit. Its diagnostic tells the caller
which operation failed and what it accepts:

```kio {check_exit_code=15}
module unit_only;

import __intrinsics__;

import __comptime__;

pure fn unit_impl(ct: __Comptime__, source: __Type__, value: __Checked_term__) -> __Checked_term__ {
  __if_then_else__(
  , __type_equal__(ct, source, __type_unit__(ct))
  , .() -> __Checked_term__ { value }
  , .() -> __Checked_term__ { __elab_error__(ct, "unit_only! requires a unit source") }
  )
}

elab unit_only : [A] A -> . { impl unit_impl }

fn trigger() { unit_only!(((), ())) }
```

`__elab_error__` reports elaborator error `15` at the call. For diagnostics
that depend on a reflected shape, build text with `__type_display__` and
`__diagnostic_concat__`; do not parse display text to decide type identity.

### Reject a generated result of the wrong type

This implementation returns checked unit, but its declaration promises a
pair. Checking the implementation alone cannot detect that dynamic mismatch:

```kio {check_exit_code=15}
module wrong_result;

import __comptime__;

pure fn wrong_impl(ct: __Comptime__, _source: __Type__, _value: __Checked_term__) -> __Checked_term__ {
  __term_unit__(ct)
}

elab wrong : [A] A -> (A & A) { impl wrong_impl }

fn trigger() { let _pair = wrong!(()); () }
```

The bang call rejects the returned term with elaborator error `15`. Checked
construction ensures the unit term itself is valid; it does not make it
satisfy every declared elaborator result.

### Keep a captured host call at runtime

Capturing a host function is allowed. Emitting its call into a pure consumer
is a separate violation:

```kio {check_exit_code=14}
module generated_purity;

import __comptime__;

host fn action();

pure fn emit_impl(ct: __Comptime__, action_term: __Checked_term__) -> __Checked_term__ {
  __term_call__(ct, __term_type__(ct, action_term), action_term, __term_unit__(ct))
}

elab emit : . -> . { captures action; impl emit_impl }

pure fn trigger() { emit!() }
```

An ordinary `fn trigger` can contain that generated host call. A direct
executable reference in the implementation is rejected instead:

```kio {check_exit_code=14}
module direct_host_execution;

import __comptime__;

host fn action();

pure fn execute_impl(ct: __Comptime__) -> __Checked_term__ { action(); __term_unit__(ct) }

elab execute : . -> . { impl execute_impl }

fn trigger() { execute!() }
```

The error points to `action()` inside the pure implementation. Capture its
checked reference when the desired behavior belongs to the generated runtime
program.

## Verify behavior and inspect the generated program

Run `kio check` after each implementation and its caller. Then run `kio test`:
the positive package above contains eight laws. They invoke every worked
generator and check repetition, nested mirroring, target inference, fill
agreement, and block execution. See [Testing with `equiv`](equiv.md) for what
these host-independent claims prove.

For a separate negative behavior test, this claim checks successfully but
gets the mirror's result wrong:

<!--kio {harness=wrong_law file placeholder="__SNIPPET__"}
module author/wrong_law;

import author/mirror(mirror);

__SNIPPET__
-->

```kio {@wrong_law}
equiv wrong_mirror[A](left: A, right: A) {
  mirror!((left, right));
  (left, right)
}
```

The symbolic parameters `left` and `right` are independent values of the same
type. `kio check` accepts both arms, while `kio test` rejects their different
behavior with exit `50`. Keep this rejected claim outside the successful
package. A passing type check is not a behavior test.

Finally, run `kio build kio-prime` and open the generated consumer modules
under `out/kio-prime/`. In the duplication consumer, find the generated let,
lambda, and pair. In the template consumer, find the specialized `repeat`
reference. In the mirror consumer, find projections and reversed pairs. In
the block consumer, find the thunk or nested bind calls. Bang calls and
reflection values have disappeared; the generated program has also passed
standalone Kio' validation. [Debugging with Kio'](debugging-with-kio-prime.md)
explains this inspection loop.

For reuse, expose the elaborator and its required runtime captures, keep
compile-time implementation details private, and document the supported
input shapes and errors. The [elaborator library case study](../poc/elab.md)
shows larger dispatch and deriving algorithms; [Using libraries](using-libraries.md#the-elaborator-library)
explains adopting that library. The examples on this page are complete without
its source files or a network fetch.

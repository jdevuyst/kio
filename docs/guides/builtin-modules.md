# Builtin Modules

Kio has two compiler-provided import blocks. `__intrinsics__` exposes the Kio' core value operations. `__comptime__` exposes the reflection and checked-term helpers used by elaborator implementations. Ordinary source should start with surface forms — tuples, labels, `match!`, and structural elaborators — and use `__intrinsics__` only when the core operation itself is the subject.

If you are trying to construct or inspect sums and products in a normal
program, use the structural guides instead of this reference. The
`import __intrinsics__;` snippet below is a reference entry for core-level code,
not a package or module starter.

Import and signature blocks are reference fragments and are marked `{ignore}`. Worked examples use standalone checked fences. The first paragraph of each entry is the concise summary used by editor hover; the following text is the long-form reference.

## `__intrinsics__`

Compiler-provided module containing the Kio' core value intrinsics.

```kio {ignore}
import __intrinsics__;
```

### Value Intrinsics

#### `__left__`

```kio {ignore}
__left__ : [a][b] a -> a | b
```

Constructs the left branch of a sum value.

The two type arguments fix the complete `A | B` result and the value must have type `A`. No branch search occurs. A missing or incompatible type argument is a type error.

#### `__right__`

```kio {ignore}
__right__ : [a][b] b -> a | b
```

Constructs the right branch of a sum value.

The two type arguments fix the complete `A | B` result and the value must have type `B`. No branch search occurs. A missing or incompatible type argument is a type error.

#### `__either__`

```kio {ignore}
__either__ : [a][b][c] ((a | b) & (a -> c) & (b -> c)) -> c
```

Eliminates a sum by applying the left handler or the right handler to the carried value.

The scrutinee must have type `A | B`; the two handlers receive `A` and `B` respectively and must return one common `C`. Only the selected handler runs. An uncovered type, a mismatched handler parameter, or different handler results is a type error.

#### `__pair__`

```kio {ignore}
__pair__ : [a][b] (a & b) -> a & b
```

Constructs a product value from its two fields.

The two argument types determine the ordered product `A & B`. This constructor neither flattens nor reorders nested products. Wrong explicit type arguments are rejected as type errors.

#### `__fst__`

```kio {ignore}
__fst__ : [a][b] (a & b) -> a
```

Projects the first field from a product value.

The value must have exactly the product type `A & B`; the result is its left component. The helper does not search a wider product spine, so a different product shape is a type error.

#### `__snd__`

```kio {ignore}
__snd__ : [a][b] (a & b) -> b
```

Projects the second field from a product value.

The value must have exactly the product type `A & B`; the result is its right component. The helper does not search a wider product spine, so a different product shape is a type error.

#### `__if_then_else__`

```kio {ignore}
__if_then_else__ : [A](Bool & (. -> A) & (. -> A)) -> A
```

Branches on the single in-scope `role(bool)` identity. The true and false branches are thunks so only the selected branch is evaluated.

The condition uses the unique selected `role(bool)` type identity from unqualified lexical scope. Direct host types are selected when present; aliases form the fallback pool otherwise, with repeated paths to one declaration counted once. A qualified module import alone does not add its members. Both branches are unit-domain functions returning the same `A`; the intrinsic calls only the chosen thunk. With no Boolean-role identity, more than one selected identity, a non-boolean condition, or disagreeing branch results, typechecking fails.

#### `__absurd__`

```kio {ignore}
__absurd__ : [a] ! -> a
```

Eliminates a bottom value into any requested result type.

The argument must have bottom type `!`; because `!` has no values, a well-typed closed program cannot reach this eliminator with a constructed value. It supplies no fallback behavior: an argument of any inhabited type is rejected, and a stuck host computation returning `!` remains opaque during compile-time evaluation.

## `__comptime__`

Compiler-provided block import containing reflected type handles, checked-term constructors, diagnostic text, and compile-time recursion helpers for elaborator implementations.

```kio {ignore}
import __comptime__;
```

### Types

#### `__Comptime__`

```kio {ignore}
type __Comptime__
```

Opaque proof that a helper is callable only during compile-time evaluation.

The compiler supplies this value as the first implementation argument and every `__comptime__` value helper requires it first. Source cannot construct one. Letting it reach runtime is invalid; its runtime representation is bottom.

#### `__Type__`

```kio {ignore}
type __Type__
```

Opaque reflected handle for a Kio type.

A handle preserves the identity, binders, kind arity, and structural shape of the reflected type. It is inspected and rebuilt only through this module's helpers. It is not a runtime type value, and using it outside compile-time evaluation is invalid.

#### `__Checked_term__`

```kio {ignore}
type __Checked_term__
```

Opaque checked term template. The carried type can be read with `__term_type__`.

The template is already typed Kio' syntax, not source text or an unchecked AST. Constructors verify their component types while building it, and the elaborator result is checked against the declared call result before substitution. It cannot be manufactured or inspected at runtime.

#### `__Type_var__`

```kio {ignore}
type __Type_var__
```

Opaque handle for a reflected type variable.

A handle identifies one fresh reflected binder and records that binder's kind arity. Compare handles with `__type_var_equal__` and turn one into a type with `__type_var__`. It has no user-constructible or runtime representation.

#### `__Type_name__`

```kio {ignore}
type __Type_name__
```

Opaque handle for the named head of a reflected named type.

A handle identifies the named head of a reflected path together with its parameter arities. Use `__type_name_type__` before applying arguments. It is not interchangeable with `__Type__` and has no runtime representation.

#### `__Type_arity__`

```kio {ignore}
type __Type_arity__
```

Opaque arity value used by reflected type variables, names, and binders.

Arity is a compile-time natural number used for kinds and function parameter packets. Build it from zero and successor or read it from a type/variable. It is opaque to ordinary arithmetic and has no runtime representation.

#### `__Diagnostic_text__`

```kio {ignore}
type __Diagnostic_text__
```

Compile-time text used in elaborator diagnostics.

Diagnostic text is the only message payload accepted by `__elab_error__` and `__type_error__`. Produce it from `Comptime_str`, display helpers, and concatenation. It is compiler-owned text, not a runtime string.

#### `Comptime_bool`

```kio {ignore}
type Comptime_bool
```

Compile-time boolean type with `role(bool)`.

The literals `.t` and `.f` form the compile-time predicate result used by reflection helpers and `if`/`else`. It is distinct from every package host boolean. It is erased after compile-time evaluation and is not a runtime API type.

#### `Comptime_str`

```kio {ignore}
type Comptime_str
```

Compile-time string type with `role(str)`.

String literals in an elaborator implementation can inhabit this compile-time role string and flow into diagnostic text. It is distinct from package host strings, is erased after evaluation, and cannot cross into generated runtime code.

#### `__Fill_ctx__`

```kio {ignore}
type __Fill_ctx__
```

Opaque immutable transcript of explicit fill relations for one marked call.

The compiler supplies one fresh context as the second argument of an `impl(fills)` implementation. `__fill__` returns a descendant context instead of mutating its input, so Kio code threads the returned value explicitly. A context cannot be constructed by source, converted to or from `__Comptime__`, inspected, or allowed to reach runtime.

### Type Inspection

#### `__reflect_type__`

```kio {ignore}
__reflect_type__ : __Comptime__ -> [a] __Type__
```

Reflects a type argument into an opaque `__Type__` handle. Elaborators use it when they need to inspect a type parameter, compare types, or pass a type into checked-term construction.

The type argument must be solved before the helper runs; the result is its exact reflected handle, including nominal identity and kind. An unsolved elaborator type slot cannot call this helper and is reported as a type-inference error before evaluation.

#### `__type_view__`

```kio {ignore}
__type_view__ : (__Comptime__ & __Type__) ->
  | .
  | .
  | (__Type__ & __Type__)
  | (__Type__ & __Type__)
  | (__Type__ & __Type_arity__ & __Type__)
  | (__Type_var__ & __Type_arity__ & __Type__)
  | __Type_var__
  | __Type_name__
```

Decomposes a reflected type into the compiler's structural type-view shape.

After transparent-alias unfolding, returns an eight-arm structural view in this order: unit; bottom; product pair; sum pair; function `(parameters & arity & result)`; forall `(variable & arity & body)`; type variable; named head. Consumers must cover the complete sum. An unresolved type cannot be viewed and leaves elaboration stuck.

#### `__type_is_host__`

```kio {ignore}
__type_is_host__ : (__Comptime__ & __Type__) -> Comptime_bool
```

Tests whether a reflected type is a host type — one declared with `host type`.

Returns `.t` exactly when the reflected identity resolves to a package `host type`, following transparent aliases, and `.f` for structural or package-defined nominal types. The argument must be a resolved reflected type; the helper emits no diagnostic sentinel.

#### `__type_host_converters__`

```kio {ignore}
__type_host_converters__ : (__Comptime__ & __Type__ & __Type__) -> __Type__
```

Builds the converter product type needed to turn host leaves inside a source type into compile-time strings.

Builds the right-associated product of converter function types needed for every host-type leaf reachable in `source_type`; each converter maps that host leaf (polymorphically when needed) to `string_type`. No host leaves produce unit. A converter value is checked separately by `__term_host_convert__`; mismatch there makes term construction fail.

#### `__type_equal__`

```kio {ignore}
__type_equal__ : (__Comptime__ & __Type__ & __Type__) -> Comptime_bool
```

Tests whether two reflected types are equivalent.

Returns a compile-time boolean using Kio's definitional type equivalence, including transparent-alias unfolding and binder renaming while preserving nominal and host identities. It never coerces structural near-matches; unresolved handles cannot reach the helper.

#### `__type_name_equal__`

```kio {ignore}
__type_name_equal__ : (__Comptime__ & __Type_name__ & __Type_name__) -> Comptime_bool
```

Tests whether two reflected type-name handles name the same type head.

Returns `.t` only when both reflected named heads have the same qualified identity and parameter-arity vector. Applied arguments are not compared here. Supplying ordinary type handles instead of name handles is a type error.

#### `__type_var_equal__`

```kio {ignore}
__type_var_equal__ : (__Comptime__ & __Type_var__ & __Type_var__) -> Comptime_bool
```

Tests whether two reflected type-variable handles name the same variable.

Returns `.t` only when both handles identify the same reflected binder with the same arity. Equal display names from different binders do not suffice. Supplying ordinary type handles instead of variable handles is a type error.

#### `__type_is_product_root__`

```kio {ignore}
__type_is_product_root__ : (__Comptime__ & __Type__) -> Comptime_bool
```

Tests whether a reflected type's outer shape is a product spine.

Unfolds transparent aliases and tests only the outer constructor. Nested products below another constructor do not count. The result is always a compile-time boolean for a resolved handle.

#### `__type_is_sum_root__`

```kio {ignore}
__type_is_sum_root__ : (__Comptime__ & __Type__) -> Comptime_bool
```

Tests whether a reflected type's outer shape is a sum spine.

Unfolds transparent aliases and tests only the outer constructor. Nested sums below another constructor do not count. The result is always a compile-time boolean for a resolved handle.

### Type Construction

#### `__type_unit__`

```kio {ignore}
__type_unit__ : __Comptime__ -> __Type__
```

Constructs the reflected unit type.

Returns the reflected unit type `.`. It takes only the compile-time proof and cannot fail once called in a compile-time context.

#### `__type_bottom__`

```kio {ignore}
__type_bottom__ : __Comptime__ -> __Type__
```

Constructs the reflected bottom type.

Returns the reflected bottom type `!`. It constructs a type handle, not a bottom value, and cannot fail once called in a compile-time context.

#### `__type_product__`

```kio {ignore}
__type_product__ : (__Comptime__ & __Type__ & __Type__) -> __Type__
```

Constructs a reflected product type from two reflected member types.

Constructs the ordered binary type `left & right`; it does not flatten either input. Both inputs must already be reflected types, so malformed components are rejected by the helper's static scheme.

#### `__type_sum__`

```kio {ignore}
__type_sum__ : (__Comptime__ & __Type__ & __Type__) -> __Type__
```

Constructs a reflected sum type from two reflected branch types.

Constructs the ordered binary type `left | right`; it does not flatten either input or remove duplicates. Both inputs must already be reflected types, so malformed components are rejected by the static scheme.

#### `__type_arrow__`

```kio {ignore}
__type_arrow__ : (__Comptime__ & __Type__ & __Type_arity__ & __Type__) -> __Type__
```

Constructs a reflected function type from a parameter packet, parameter arity, and result type.

Constructs a function type from a parameter packet, the packet's ABI arity, and a result type. The arity must describe the parameter packet the generated call convention will use; inconsistent shapes are rejected when a checked function or call is constructed.

#### `__type_forall__`

```kio {ignore}
__type_forall__ : (__Comptime__ & __Type_arity__ & (__Type_var__ -> __Type__)) -> __Type__
```

Constructs a reflected polymorphic type by binding fresh type variables.

Creates a fresh type variable of the requested kind arity, passes its handle to `body`, and binds the returned reflected type. The binder is hygienic. A body that does not return `__Type__` is rejected by the static scheme.

#### `__type_var__`

```kio {ignore}
__type_var__ : (__Comptime__ & __Type_var__) -> __Type__
```

Turns a reflected type-variable handle into a reflected type.

Turns a reflected variable handle into the type that refers to that binder. The handle must come from reflection or a `__type_forall__` callback; arbitrary names cannot be forged.

#### `__type_name_type__`

```kio {ignore}
__type_name_type__ : (__Comptime__ & __Type_name__) -> __Type__
```

Turns a reflected named-type handle into a reflected type head.

Turns a reflected name handle into its unapplied named type head. Required arguments are added with `__type_apply__`; leaving a higher-arity head unsaturated is rejected when a kind-`*` type is required.

#### `__type_apply__`

```kio {ignore}
__type_apply__ : (__Comptime__ & __Type__ & __Type__) -> __Type__
```

Applies a reflected type head to one reflected type argument.

Appends one reflected argument to a named/path type head. Apply once per argument in source order and respect the head's declared kinds. Applying a non-head or over-applying produces an unusable type that later kind/type validation rejects; this helper has no diagnostic-result arm. During marked evaluation, if the outer head is still unresolved after alias unfolding, evaluation remains residual: the helper does not return the unchanged head or discard the argument.

#### `__type_instantiate__`

```kio {ignore}
__type_instantiate__ : (__Comptime__ & __Type__ & __Type__) -> __Type__ | __Diagnostic_text__
```

Instantiates a reflected polymorphic type with one reflected type argument, returning a diagnostic on failure.

Instantiates the outermost reflected `forall` binder with one argument. Success is the left `__Type__` arm; a non-forall input or a structural argument for a higher-kinded binder returns diagnostic text in the right arm. Call again for additional binders.

#### `__type_arity__`

```kio {ignore}
__type_arity__ : (__Comptime__ & __Type__) -> __Type_arity__
```

Reads the arity of a reflected type.

Returns the kind arity of the reflected type's outer binder/head: zero for an ordinary kind-`*` type and the recorded arrow count for a higher-kinded binder. The argument must already be resolved.

#### `__type_var_arity__`

```kio {ignore}
__type_var_arity__ : (__Comptime__ & __Type_var__) -> __Type_arity__
```

Reads the arity carried by a reflected type variable.

Reads the kind arity recorded on a reflected variable handle. It cannot accept a general type handle; that mismatch is a type error.

#### `__type_arity_zero__`

```kio {ignore}
__type_arity_zero__ : __Comptime__ -> __Type_arity__
```

Constructs the zero arity value.

Constructs arity zero, representing kind `*` or an empty ABI parameter packet. It cannot fail in a compile-time context.

#### `__type_arity_succ__`

```kio {ignore}
__type_arity_succ__ : (__Comptime__ & __Type_arity__) -> __Type_arity__
```

Constructs the successor of an arity value.

Constructs the successor of an existing arity. Repeated calls encode higher-kinded arrow counts or function ABI slots; ordinary numeric values are not accepted.

#### `__type_arity_equal__`

```kio {ignore}
__type_arity_equal__ : (__Comptime__ & __Type_arity__ & __Type_arity__) -> Comptime_bool
```

Tests whether two reflected arity values are equal.

Returns a compile-time boolean comparing two opaque arity values numerically. It does not compare the types or binders those arities came from.

### Type Folds

#### `__type_product_spine_fold__`

```kio {ignore}
__type_product_spine_fold__ : __Comptime__ -> [r] (__Type__ & r & ((r & __Type__) -> r)) -> r
```

Folds over the member types of a reflected product spine.

Unfolds aliases, walks a right-associated product from left to right, and calls `step(acc, member)` for each member. A non-product is treated as a one-member spine. `R` fixes one accumulator/result type; a step with another shape is a type error.

##### Checked example: `__type_product_spine_fold__`

```kio {}
module builtin_type_fold_example;

import __comptime__;

fn product_arity(ct: __Comptime__, typ: __Type__) -> __Type_arity__ {
  __type_product_spine_fold__(
    , ct
    , __Type_arity__
    , typ
    , __type_arity_zero__(ct)
    , .(count: __Type_arity__, _member: __Type__) -> __Type_arity__ {
        __type_arity_succ__(ct, count)
      }
    )
}
```

#### `__type_sum_spine_fold__`

```kio {ignore}
__type_sum_spine_fold__ : __Comptime__ -> [r] (__Type__ & r & ((r & __Type__) -> r)) -> r
```

Folds over the branch types of a reflected sum spine.

Unfolds aliases, walks a right-associated sum from left to right, and calls `step(acc, branch)` for each branch. A non-sum is treated as a one-branch spine. `R` fixes one accumulator/result type; a step with another shape is a type error.

#### `__type_args_fold__`

```kio {ignore}
__type_args_fold__ : __Comptime__ -> [r] (__Type__ & r & ((r & __Type__) -> r)) -> r
```

Folds over the type arguments applied to a reflected type head.

Walks the applied arguments of a reflected named type from left to right and calls `step(acc, argument)` for each. A structural or unapplied type has no arguments and returns `init`. During marked evaluation, if the outer type is still unresolved after alias unfolding, evaluation remains residual: the helper does not treat it as a known zero-argument type or return `init`. The accumulator type `R` cannot change between steps.

#### `__type_function_params_fold__`

```kio {ignore}
__type_function_params_fold__ : __Comptime__ -> [r] (__Type__ & r & ((r & __Type__) -> r)) -> r
```

Folds over the parameter packet of a reflected function type.

Unfolds aliases, splits the outer function's parameter packet according to its ABI arity, and folds parameters left to right. A non-function yields no parameters and therefore `init`; malformed arity metadata prevents checked-term construction later.

#### `__type_arity_fold__`

```kio {ignore}
__type_arity_fold__ : __Comptime__ -> [r] (__Type_arity__ & r & (r -> r)) -> r
```

Runs a step function once for each slot in a reflected arity.

Calls `step(acc)` exactly once per slot in `arity`, starting from `init`. It is the total iterator for opaque arities; the accumulator must retain type `R`, and there is no early-exit channel.

#### `__type_name_param_arities_fold__`

```kio {ignore}
__type_name_param_arities_fold__ : __Comptime__ -> [r] (__Type_name__ & r & ((r & __Type_arity__) -> r)) -> r
```

Folds over the parameter arities declared by a reflected type name.

Walks a reflected name's declared parameter arities in source order and calls `step(acc, arity)`. It describes the head's parameter kinds, not currently-applied arguments. A general `__Type__` must first be viewed to obtain the name handle.

### Diagnostics

#### `__type_display__`

```kio {ignore}
__type_display__ : (__Comptime__ & __Type__) -> __Diagnostic_text__
```

Renders a reflected type as diagnostic text.

Pretty-prints the complete reflected type for a call-site diagnostic, preserving structure and qualified identities where required. Canonical qualified identities separate module-path components with `/` and the final named head with `.`, as in `foo/bar.Item`. The text is diagnostic-only and must not be parsed back into a type.

#### `__type_short_name__`

```kio {ignore}
__type_short_name__ : (__Comptime__ & __Type__) -> __Diagnostic_text__
```

Renders a compact diagnostic name for a reflected type.

Pretty-prints the complete reflected type using the evaluation module's lexical imports. An unshadowed type declared in that module or selectively imported there uses its bare name. Otherwise, the shortest unshadowed qualified import alias for the type's exact declaring module is used, with import order breaking equal-length ties; when no such spelling is available, the canonical path remains, with `/` between its module components. Type applications, functions, products, sums, forall binders, and alias heads stay intact. The text is diagnostic-only and must not be parsed or compared as a type identity; use `__type_equal__` for decisions.

#### `__type_name_display__`

```kio {ignore}
__type_name_display__ : (__Comptime__ & __Type_name__) -> __Diagnostic_text__
```

Renders a reflected type-name handle as diagnostic text.

Renders a reflected named head as diagnostic text, including its qualifying path. Canonical module-path components use `/` and the final named head uses `.`, as in `foo/bar.Item`; a lexical import alias remains source-shaped, as in `m.Item`. It does not render applied arguments and must not be used for name equality.

#### `__type_var_display__`

```kio {ignore}
__type_var_display__ : (__Comptime__ & __Type_var__) -> __Diagnostic_text__
```

Renders a reflected type-variable handle as diagnostic text.

Renders the compiler-chosen name of a reflected type variable for diagnostics. Binder identity must still be compared with `__type_var_equal__`, because display text is not an identity token.

#### `__type_arity_display__`

```kio {ignore}
__type_arity_display__ : (__Comptime__ & __Type_arity__) -> __Diagnostic_text__
```

Renders a reflected arity value as diagnostic text.

Renders the opaque arity as decimal diagnostic text. This is presentation only; arity logic uses the constructor, fold, and equality helpers.

#### `__diagnostic_concat__`

```kio {ignore}
__diagnostic_concat__ : (__Comptime__ & __Diagnostic_text__ & __Diagnostic_text__) -> __Diagnostic_text__
```

Concatenates two pieces of diagnostic text.

Concatenates two diagnostic fragments without inserting whitespace or punctuation. Both inputs must be compile-time diagnostic text; host strings are rejected by the type checker.

### Error Sentinels

#### `__elab_error__`

```kio {ignore}
__elab_error__ : (__Comptime__ & __Diagnostic_text__) -> __Checked_term__
```

Builds a checked-term sentinel that reports an elaborator diagnostic at the call site.

Returns a checked-term error sentinel carrying the message. When selected as the elaborator result, the call fails in the elaborator-error category (exit 15) at the bang-call site. It does not construct a runtime term.

#### `__type_error__`

```kio {ignore}
__type_error__ : (__Comptime__ & __Diagnostic_text__) -> __Checked_term__
```

Builds a checked-term sentinel that reports a type error at the call site.

Returns a checked-term error sentinel carrying the message. When selected as the elaborator result, the call fails in the ordinary type-error category (exit 14) at the bang-call site. It does not construct a runtime term.

### Compile-Time Recursion

#### `__structural_recur__`

```kio {ignore}
__structural_recur__ : __Comptime__ -> [fuel][input][result] (& fuel
& input
& ((((fuel & input) -> result) & fuel & input) -> result)) -> result
```

Runs fuel-checked compile-time structural recursion through an ordinary helper call.

Runs `step(recur, fuel, input)` after measuring the initial `fuel`. Unit, literals, structurally determined reflected values, arities, diagnostic text, checked terms, and finite constructor trees with measurable children are measurable; closures, opaque atoms, stuck terms, case splits, and recursive callbacks are not. An initial projected type uses its authenticated alias-unfolded structural carrier as a conservative lower bound; its exact scoped snapshot remains available for whole-handle reflection and fills. An unresolved outer carrier is unmeasurable, and a direct projected value used on a recursive edge must be complete. When an ordinary finite constructor carries a projected field, a known outer structure gives that field the same conservative size in initial and recursive measurements; an unresolved outer field makes the constructor unmeasurable. A structurally determined child can remain measurable even when its parent is open. `recur(next_fuel, next_input)` must give the callback a recursive fuel measure strictly below both the current measure and its stored root measure. Evaluator-created callbacks maintain `current ≤ root`; both bounds are checked independently for a callback constructed directly through the evaluator API. Each resolved helper source occurrence supplies an origin that survives proof/type application stages and higher-order use. The helper's three operands and the callback's two operands may be written as flattened arguments or as one right-folded product packet. After all operands are evaluated, the first invocation at an origin uses the initial measure; a re-entry at that origin uses the recursive measure and must be strictly below the nearest active invocation. Invocations from distinct origins do not compare measures. Equal-measure fuel is rejected regardless of value identity, representation, or whether it contains a projected handle. The callback has type `(fuel & input) -> result`, and every path returns the one `result` type. Unmeasurable initial or recursive fuel, a callback that does not strictly descend from its current measure or stay below its stored root, or an active same helper origin re-entry that does not strictly descend leaves the evaluator stuck and is reported as a totality error (exit 16) with root/current/next measures when available. If both callback bounds fail, the diagnostic reports the current edge; the root-specific diagnostic applies only when the current edge descends but the root bound fails. Once an evaluated edge fails, enclosing compile-time computation cannot discard it; that evaluation reports its first totality failure.

##### Checked example: `__structural_recur__`

```kio {}
module builtin_structural_recur_example;

import __intrinsics__;

import __comptime__;

rec newtype Fuel : . | Fuel { pub constructor mk_fuel; pub projector un_fuel }

fn stop() -> Fuel { Fuel.mk_fuel(__left__(., Fuel, ())) }

fn tick(rest: Fuel) -> Fuel { Fuel.mk_fuel(__right__(., Fuel, rest)) }

fn count_down(ct: __Comptime__, fuel: Fuel) -> __Type_arity__ {
  __structural_recur__(
    , ct
    , Fuel
    , .
    , __Type_arity__
    , fuel
    , ()
    , .(recur: (Fuel & .) -> __Type_arity__, current: Fuel, _input: .) -> __Type_arity__ {
        __either__(
          , .
          , Fuel
          , __Type_arity__
          , Fuel.un_fuel(current)
          , .(_done: .) -> __Type_arity__ { __type_arity_zero__(ct) }
          , .(rest: Fuel) -> __Type_arity__ { __type_arity_succ__(ct, recur(rest, ())) }
          )
      }
    )
}
```

### Checked Terms

#### `__term_type__`

```kio {ignore}
__term_type__ : (__Comptime__ & __Checked_term__) -> __Type__
```

Reads the reflected type carried by a checked term.

Reads the exact reflected type stored on a checked term, qualifying host identities consistently with reflected call types. Error sentinels and non-term compile-time values cannot be inspected as terms.

#### `__term_let__`

```kio {ignore}
__term_let__ : (__Comptime__ & __Type__ & __Checked_term__ & (__Checked_term__ -> __Checked_term__)) -> __Checked_term__
```

Constructs a checked let term.

Creates a hygienic let binder of `value_type`, checks `value` against it, passes a checked reference to `body`, and returns the body's checked term. A value/type mismatch or non-term callback result makes construction fail and the elaborator cannot return a valid term.

#### `__term_fn__`

```kio {ignore}
__term_fn__ : (__Comptime__ & __Type__ & (__Checked_term__ -> __Checked_term__)) -> __Checked_term__
```

Constructs a checked function term.

Creates a hygienic value lambda with the displayed `fn_type`; the callback receives one checked parameter packet and must return a term of the function's result type. The type must be an outer function with consistent ABI arity, otherwise construction stays stuck and the elaborator fails.

##### Checked example: `__term_fn__`

```kio {}
module builtin_checked_fn_example;

import __comptime__;

fn identity_term(ct: __Comptime__, typ: __Type__) -> __Checked_term__ {
  let fn_type = __type_arrow__(ct, typ, __type_arity_succ__(ct, __type_arity_zero__(ct)), typ);
  __term_fn__(ct, fn_type, .(parameter: __Checked_term__) -> __Checked_term__ { parameter })
}
```

#### `__term_call__`

```kio {ignore}
__term_call__ : (__Comptime__ & __Type__ & __Checked_term__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked function call term.

Creates a checked application after splitting `fn_type` into its parameter packet and result. `fn_value` must have that exact function type and `arg_packet` must have the right-associated parameter type; any mismatch makes construction stay stuck rather than emitting ill-typed Kio'.

#### `__term_type_fn__`

```kio {ignore}
__term_type_fn__ : (__Comptime__ & __Type_arity__ & (__Type_var__ -> __Checked_term__)) -> __Checked_term__
```

Constructs a checked type-function term.

Creates a hygienic type abstraction with a fresh binder of `var_arity`; the callback receives its variable handle and returns the checked body. The result is polymorphic checked Kio'. A non-term callback result cannot be constructed.

#### `__term_type_app__`

```kio {ignore}
__term_type_app__ : (__Comptime__ & __Checked_term__ & __Type__) -> __Checked_term__
```

Constructs a checked type-application term.

Applies one reflected type argument to a checked term whose outer type is `forall`, substituting the binder in the result type. A monomorphic term or invalid kind application makes construction stay stuck.

#### `__term_unit__`

```kio {ignore}
__term_unit__ : __Comptime__ -> __Checked_term__
```

Constructs the checked unit term.

Constructs the checked unit term `()` with reflected type `.`. It is always valid when called with the compile-time proof.

#### `__term_host_convert__`

```kio {ignore}
__term_host_convert__ : (__Comptime__ & __Type__ & __Type__ & __Checked_term__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked host-conversion term using the converter product from `__type_host_converters__`.

Selects the converter matching the host-typed leaf carried by `value` and constructs its checked call to `string_type`. `converters` must have exactly the product type returned by `__type_host_converters__`; an absent host match, wrong converter product, or wrong value type makes construction fail.

#### `__term_specialize__`

```kio {ignore}
__term_specialize__ : (__Comptime__ & __Checked_term__ & __Type__ & __Type__) ->
  __Checked_term__ | . | __Diagnostic_text__
```

Specializes an explicitly polymorphic checked term against a reflected type relation.

Uses only its explicit checked term, pattern type, and target type. Success returns the left checked-term arm, an ordinary route mismatch returns unit, and malformed or underdetermined operands return diagnostic text. It receives no fill context and cannot inspect the call's transcript or ambient inference state.

##### Checked example: `__term_specialize__`

```kio {}
module builtin_term_specialize_example;

import __comptime__;

fn specialize(ct: __Comptime__, term: __Checked_term__, pattern: __Type__, target: __Type__) ->
  __Checked_term__ | . | __Diagnostic_text__
   { __term_specialize__(ct, term, pattern, target) }
```

### Reflected Intrinsic Constructors

#### `__intrinsic_pair__`

```kio {ignore}
__intrinsic_pair__ : (__Comptime__ & __Checked_term__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked term that calls the reflected product constructor intrinsic.

Constructs a checked call to runtime `__pair__` and derives the product type from the two checked operands. This is a reflected-term constructor; it does not call the runtime intrinsic during elaboration. Non-term operands are type errors at the helper call.

#### `__intrinsic_fst__`

```kio {ignore}
__intrinsic_fst__ : (__Comptime__ & __Type__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked term that calls the reflected first-projection intrinsic.

Constructs a checked runtime first projection. `product_type` must unfold to `A & B` and `value` must have that exact type; otherwise construction fails instead of emitting an invalid projection.

#### `__intrinsic_snd__`

```kio {ignore}
__intrinsic_snd__ : (__Comptime__ & __Type__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked term that calls the reflected second-projection intrinsic.

Constructs a checked runtime second projection. `product_type` must unfold to `A & B` and `value` must have that exact type; otherwise construction fails instead of emitting an invalid projection.

#### `__intrinsic_left__`

```kio {ignore}
__intrinsic_left__ : (__Comptime__ & __Type__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked term that calls the reflected left-sum constructor intrinsic.

Constructs a checked runtime left injection. `sum_type` must unfold to `A | B` and `value` must have type `A`; mismatches make construction fail.

#### `__intrinsic_right__`

```kio {ignore}
__intrinsic_right__ : (__Comptime__ & __Type__ & __Checked_term__) -> __Checked_term__
```

Constructs a checked term that calls the reflected right-sum constructor intrinsic.

Constructs a checked runtime right injection. `sum_type` must unfold to `A | B` and `value` must have type `B`; mismatches make construction fail.

#### `__intrinsic_either__`

```kio {ignore}
__intrinsic_either__ : (& __Comptime__
& __Type__
& __Type__
& __Checked_term__
& (__Checked_term__ -> __Checked_term__)
& (__Checked_term__ -> __Checked_term__)) -> __Checked_term__
```

Constructs a checked term that calls the reflected sum eliminator intrinsic.

Constructs a checked runtime sum elimination. The value must have `sum_type`; each callback receives a checked branch payload and must return `result_type`. A non-sum, wrong scrutinee, or handler-result mismatch makes construction fail.

#### `__intrinsic_absurd__`

```kio {ignore}
__intrinsic_absurd__ : (__Comptime__ & __Checked_term__ & __Type__) -> __Checked_term__
```

Constructs a checked term that calls the reflected bottom eliminator intrinsic.

Constructs a checked runtime bottom elimination from `bottom_value` to `result_type`. The value must have type `!`; any inhabited source type makes construction fail.

#### `__intrinsic_if_then_else__`

```kio {ignore}
__intrinsic_if_then_else__ : (__Comptime__ & __Type__ & __Checked_term__ & (. -> __Checked_term__) & (. -> __Checked_term__)) -> __Checked_term__
```

Constructs a checked term that calls the reflected role-boolean conditional intrinsic.

Constructs a checked runtime `__if_then_else__` call. The condition must use the caller's single `role(bool)` identity and both zero-argument callbacks must return `result_type`. Missing or ambiguous Boolean-role information, or a branch mismatch, makes construction fail.

### Fill Relations

#### `__fill__`

```kio {ignore}
__fill__ : (__Comptime__ & __Fill_ctx__ & __Type__ & __Type__) -> __Fill_ctx__
```

Appends one ordered destination/candidate relation to a fill transcript.

Requires the marked call's `__Comptime__` proof first and matching `__Fill_ctx__` second. It records the relation without solving, normalizing, or reading other transcript entries; only the returned context contains the append. Relation order is therefore ordinary source-level value flow.

##### Checked example: `__fill__`

```kio {}
module builtin_fill_example;

import __comptime__;

fn append_result(ct: __Comptime__, fills: __Fill_ctx__, destination: __Type__, candidate: __Type__) -> __Fill_ctx__ {
  __fill__(ct, fills, destination, candidate)
}
```

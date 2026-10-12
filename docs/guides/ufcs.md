# UFCS calls

UFCS — Uniform Function Call Syntax — is a compact way to put one value into a function or elaborator call. It is useful when the value you are transforming reads better as the subject of the expression than as one nested argument in a prefix call.

The four surface spellings are `.>`, `.>>`, `.<`, and `.<<`. This guide calls those the dot-splice spellings because each one splices a receiver into a value-argument slot.

Assumed setup: a module whose host supplies `String` and `string_concat`. Snippets are module bodies.

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(narrow_prod);

host type String role(str);
host fn string_concat(p0: String, p1: String) -> String;

__INSERT_CODE_HERE__
-->

## The four spellings

All four forms build the same call plan as a prefix call. The spelling controls where the inserted value is written and whether it becomes the first or last value argument.

| Form | Means | Inserted value |
| --- | --- | --- |
| `x.>f(T, a)` | `f(T, x, a)` | first value slot |
| `x.>>f(T, a)` | `f(T, a, x)` | last value slot |
| `f(T, a).<x` | `f(T, a, x)` | last value slot |
| `f(T, a).<<x` | `f(T, x, a)` | first value slot |

The inserted value follows the same right-associated product rule as ordinary calls. If `x` has product type, it can fill a product parameter exactly as `f(x)` or `f((a, b))` would; dot-splice does not invent a separate receiver-passing rule. Receiver insertion happens before the resulting prefix call is checked.

For a unary splice, omit the argument list: `x.>f` means `f(x)`. A written
empty UFCS list such as `x.>f()` is rejected because it is unclear whether the
writer intended no further argument or the Unit behavior of a direct empty
call. The diagnostic shows both repairs: remove the list for the bare form, or
replace it with `(())` to pass Unit explicitly. The same rule applies in every
direction and to ordinary, member, and bang-call callees:

```kio {@module}
fn wrap(s: String) -> String { string_concat("[", string_concat(s, "]")) }

fn receiver_first(s: String) -> String { s.>wrap }

fn receiver_last(s: String) -> String { s.>>wrap }

fn argument_last(s: String) -> String { wrap.<s }

fn argument_first(s: String) -> String { wrap.<<s }
```

Thus the receiver-only spellings are `x.>f`, `x.>>f`, `f.<x`, and `f.<<x`.
An additional Unit is written `x.>f(())`, `x.>>f(())`, `f(()).<x`, or
`f(()).<<x`.

With an existing argument list, the two right-callee forms and the two left-callee forms are duals:

```kio {@module}
fn pair(a: String, b: String) -> String {
  string_concat("(", string_concat(a, string_concat(",", string_concat(b, ")"))))
}

fn receiver_first(a: String, b: String) -> String { a.>pair(b) }

fn argument_first(a: String, b: String) -> String { pair(b).<<a }

fn receiver_last(a: String, b: String) -> String { a.>>pair(b) }

fn argument_last(a: String, b: String) -> String { pair(b).<a }
```

`receiver_first(a, b)` and `argument_first(a, b)` both call `pair(a, b)`. `receiver_last(a, b)` and `argument_last(a, b)` both call `pair(b, a)`.

## What can appear on each side

`.>` and `.>>` take the value on the left and a path-shaped callee on the right:

```text
value.>f
value.>module_alias.f(arg)
value.>Type.member(arg)
value.>>f(arg)
```

The complete callee path may be grouped, including nested groups:
`x.>((helpers.f))` means `x.>helpers.f`. The formatter removes the grouping.
The right side still requires a path; calls such as these are not accepted:

```text
"a".>(f())
"a".>>(make_formatter())
```

`.<` and `.<<` take the callee on the left and the value on the right. The callee must be a path or a direct path call:

```text
f.<value
f(a).<value
module_alias.f(a).<<value
Type.member(a).<<value
```

The left side is not an arbitrary expression that happens to have a function type:

```text
scope! { make_formatter(seed) }.<value
(if! flag { first } else { second }).<<value
```

## Type arguments

UFCS calls accept explicit type arguments anywhere the equivalent prefix call would:

```kio {@module}
fn id[A](x: A) -> A { x }

fn explicit(s: String) -> String { s.>id(String) }

fn explicit_last(s: String) -> String { s.>>id(String) }
```

Type arguments stay in type slots. The receiver is never inserted into a type
slot. A `. -> R` function type always has one Unit receiver slot, regardless
of whether its declaration used `()` or one explicit Unit parameter.

When a receiver sits between two binder runs, a type-looking written argument
at that boundary belongs to the consecutive binder run immediately after the
receiver first. The binder run before the receiver consumes only any surplus.
An unfilled value layer stops that reservation, so an argument never jumps
across a missing value to reach later binders. This is determined from the
callee's unfolded function type and the written argument shapes; declaration
arity and parameter spelling do not participate, and a type mismatch does not
make the typer retry a different placement.

The ordinary Unit spellings remain distinct before and after receiver
insertion: `()` is a value argument and `.` is a type argument. UFCS never
reinterprets one as the other. If `nil : [A] . -> List(A)`, the prefix call
`nil(A)` is the residual function `. -> List(A)`. The receiver form
`().>nil(A)` inserts the written Unit receiver and is equivalent to
`nil(A, ())`; so is the explicitly nested `nil(A)()`. The receiver saturates
the UFCS call. Merely exposing a Unit domain through a type argument or
transparent alias never creates a value application.

Receiver insertion does not create a second argument-grouping rule. Prefix
and UFCS calls both walk the resolved call type from left to right: values
followed by more values fill product slots one at a time, and only the final
value in a packet may fill the whole remaining product. The typer does not
retry another packing after a mismatch. An explicit nested application remains
the way to mark a product boundary, just as in the equivalent prefix call.

## Grouping

Parentheses group UFCS expressions normally.

For `.<` and `.<<`, the right-hand value is intentionally tight: it is a primary expression plus ordinary call applications, stopping before another UFCS suffix. This keeps `f.<x.>g` readable as "call `f` with `x`, then pass that result to `g`":

```kio {@module}
fn pair(a: String, b: String) -> String {
  string_concat("(", string_concat(a, string_concat(",", string_concat(b, ")"))))
}

fn wrap(s: String) -> String { string_concat("[", string_concat(s, "]")) }

fn tight(a: String, b: String) -> String { wrap.<a.>pair(b) }

fn grouped(a: String, b: String) -> String { wrap.<(a.>pair(b)) }
```

`tight(a, b)` means `pair(wrap(a), b)`. `grouped(a, b)` means `wrap(pair(a, b))`.

## Elaborator calls

Elaborator bang-calls participate in the same rule. The typer uses the
elaborator's resolved call type, keeps explicit type arguments in type slots,
and inserts the receiver into the selected value slot.

Bang calls also keep a bare receiver-only spelling in each direction:
`value.>transform!`, `value.>>transform!`, `transform!.<value`, and
`transform!.<<value`. A post-`!` list, when present, must contain an argument.

```kio {@module}
fn drop_unit_right(p: String & .) -> String { p.>narrow_prod!(String) }

fn drop_unit_left(p: String & .) -> String { narrow_prod!(String).<<p }
```

Both functions build the same elaborator call: `narrow_prod!(p, String)`.

An elaborator with declared trailing blocks uses a complete direct block call.
For `match!`, write the scrutinee before the clause block:

```text
match! value { <clauses> }
```

## Choosing a spelling

Prefer the form that keeps the main value visible.

An ordinary nested call can read more clearly as a chain while keeping its
original argument grouping:

```kio {@module}
fn line_prefix(label: String, value: String) -> String {
  string_concat(label, string_concat(value, "\n"))
}

fn line_ufcs(label: String, value: String) -> String {
  value.>string_concat("\n").>>string_concat(label)
}
```

Both forms call `string_concat(label, string_concat(value, "\n"))`.
The inner result fills the last argument of the outer call. Choose a prefix
call when it shows the roles more clearly; changing a right-nested tree into a
left-associated chain can change evaluation order or host-visible behavior.

- Use `.>` when the value is the subject and the function naturally follows it: `row.>narrow_prod!(Name)`.
- Use `.>>` when a pipeline value belongs after already-written arguments: `msg.>>string_concat(prefix)`.
- Use `.<` when you have written a function call and want to append one value: `pair(left).<right`.
- Use `.<<` when you have written a function call and want to prepend one value: `pair(right).<<left`, or `narrow_prod!(Target).<<value`.

All four forms are surface syntax only. During typing, ordinary UFCS calls record normalized prefix calls, and elaborator UFCS calls record the same elaboration the equivalent prefix bang-call would produce.

## Converting a nested call in the editor

Place the caret in a nested ordinary call and choose **Convert nested calls to
UFCS** from the editor's refactor actions. The action converts the whole
unambiguous chain of calls to the same function, using `.>` or `.>>` at each
step to keep the original argument positions. Chains can mix the two
directions and have any number of calls, starting at two. Selecting exactly
one nested call expression limits the action to that expression.

The action works with local functions and imported or qualified names, and
does not require a warning. It keeps explicit type arguments, grouping, and
comments belonging to retained expressions. It preserves the original call
tree and evaluation stages rather than treating a function as associative.
The formatter does not perform this rewrite automatically.

If both boundary arguments continue the same chain, select one branch instead.
The action is withheld when it cannot establish the same resolved function and
argument placement, when a comment between call delimiters has no clear new
attachment, or while the document or its providers lack current typechecking
results. Computed callees, explicit staged calls such as `f(a)(b)`, elaborator
calls, and existing UFCS calls are not chain links; they may remain unchanged
inside a retained argument.

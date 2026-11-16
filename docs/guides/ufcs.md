# UFCS calls

UFCS — Uniform Function Call Syntax — is a compact way to put one value into a function or macro call. It is useful when the value you are transforming reads better as the subject of the expression than as one nested argument in a prefix call.

The four surface spellings are `.>`, `.>>`, `.<`, and `.<<`. This guide calls those the dot-splice spellings because each one splices a receiver into a value-argument slot.

Assumed setup: a module whose host supplies `String` and `string_concat`. Snippets are module bodies.

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

import spine_macros {macro narrow_prod}

host type String role(str);
host fn string_concat(p0: String, p1: String) -> String;

__INSERT_CODE_HERE__
-->

## The four spellings

All four forms build the same call plan as a prefix call. The spelling controls where the inserted value is written and whether it becomes the first or last value argument.

| Form | Means | Inserted value |
| --- | --- | --- |
| `x.>f[T](a)` | `f[T](x, a)` | first value slot |
| `x.>>f[T](a)` | `f[T](a, x)` | last value slot |
| `f[T](a).<x` | `f[T](a, x)` | last value slot |
| `f[T](a).<<x` | `f[T](x, a)` | first value slot |

The inserted value follows the same right-associated product rule as ordinary calls. If `x` has product type, it can fill a product parameter exactly as `f(x)` or `f((a, b))` would; dot-splice does not invent a separate receiver-passing rule. Receiver insertion happens before the resulting prefix call is checked.

For a unary splice, omit the argument list: `x.>f` means `f(x)`. A written
empty UFCS list such as `x.>f()` is rejected because it is unclear whether the
writer intended no further argument or the Unit behavior of a direct empty
call. The diagnostic shows both repairs: remove the list for the bare form, or
replace it with `(())` to pass Unit explicitly. The same rule applies in every
direction and to ordinary, member, and macro call callees:

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

`.<` and `.<<` select the immediately preceding outermost value call.
They can therefore operate on a call whose callee is itself computed:

```text
f(a)(b).<value       // f(a)(b, value)
f(a)(b).<<value      // f(a)(value, b)
(f(a).<value)(b)     // f(a, value)(b)
(f(a)(b)).<value     // f(a)(b, value)
```

The last example groups the whole left expression, so it selects the same
outer call. To target an earlier stage, place the splice inside the grouping,
as the third example does. To call the *result* instead, use an ordinary value
group: `f(a)(b)(value)`.

With no preceding value application, a reverse splice applies its receiver
to the selected value: `f.<value` means `f(value)`. A trailing type group is
an explicit stage boundary: `f(a)[B].<value` means `f(a)[B](value)`, without
searching backwards to insert into `(a)`.

## Type arguments

Blockless UFCS calls accept explicit type arguments wherever the equivalent
prefix call would:

```kio {@module}
fn id[A](x: A) -> A { x }

fn explicit(s: String) -> String { s.>id[String] }

fn explicit_last(s: String) -> String { s.>>id[String] }
```

Type arguments stay in type slots. The receiver is never inserted into a type
slot. A `. -> R` function type always has one Unit receiver slot, regardless
of whether its declaration used `()` or one explicit Unit parameter.

Brackets on a forward callee instantiate its leading binders before receiver
insertion: `x.>f[A](b)` means `f[A](x,b)`. If consuming `x` exposes another
binder, instantiate that result afterward: `f(x)[T]` or `(x.>f)[T]`.
The brackets never choose a type slot by searching around the receiver.
[Calls with trailing blocks](#calls-with-trailing-blocks) infer their type
arguments and do not admit explicit type arguments in the block-call spelling.

The Unit type is `.`, while `()` is a value. For
`nil : [A] . -> List[A]`, `nil[A]` retains `. -> List[A]`;
`().>nil[A]` and `nil[A]()` apply the Unit stage. Exposing a Unit domain
through substitution or a transparent alias does not create a value call.

Each selected group is one product argument for one function stage. A
receiver with product type follows ordinary tuple rules; insertion does not
split it across a returned function. For `f : (A & B) -> C -> R`,
`a.>f(b)(c)` means `f(a,b)(c)`, while `f(a,b)(c).<r` modifies the second
group and means `f(a,b)(c,r)`, subject to that group's ordinary type check.

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

User operators follow the same atom rule. With prefix `! _`, write
`f.<(!x)` rather than `f.<!x`. A final fixed symbol completes an atom, so
`< _ >` permits `f.< <x>`; in `f.< <x>.>g`, the final splice applies `g`
to the result of calling `f`. See [operator grouping](operators.md#choose-how-it-chains).

<!-- markdownlint-disable-next-line MD033 -->
<a id="macro-calls"></a>

## Macro calls

Macro calls participate in the same rule. The typer uses the
macro's resolved call type, keeps explicit type arguments in type slots,
and inserts the receiver into the selected value slot.

Macro calls also keep a bare receiver-only spelling in each direction:
`value.>transform`, `value.>>transform`, `transform.<value`, and
`transform.<<value`. The UFCS argument list, when present, must contain an argument.

```kio {@module}
fn drop_unit_right(p: String & .) -> String { (p.>narrow_prod)[String] }

fn drop_unit_left(p: String & .) -> String { (narrow_prod.<<p)[String] }
```

Both functions build the same macro call: `narrow_prod(p)[String]`.

### Calls with trailing blocks

Trailing blocks stay attached to the macro in every direction. The receiver
enters a prefix group **before the blocks**, even when written after them.
Forward forms select the first prefix group and reverse forms the last.
With no prefix group, insertion creates a receiver-only prefix group:

| Spelling | Means |
| --- | --- |
| `r.>f(a) { body }` | `f(r, a) { body }` |
| `r.>>f(a) { body }` | `f(a, r) { body }` |
| `f(a) { body }.<<r` | `f(r, a) { body }` |
| `f(a) { body }.<r` | `f(a, r) { body }` |

Here `f` stands for an imported macro with the corresponding ordinary values
and trailing block. The final prefix value and each whole projected block
value form one right-associated product for one function stage; earlier prefix
groups remain separate stages. Each spelling has the same call plan and
runtime behavior as the equivalent direct call. Macro inputs are quoted; the
generated code determines whether and how they run.

For example, the library `match` macro accepts a scrutinee and a clause block.
Here its module is available as `match`; use your configured dependency path
when [adopting the library](using-libraries.md#the-macro-library).
This complete module defines two functions that supply exactly the same arguments:

```kio {}
mod block_splices;

import match { macro match }

host type String role(str);

fn receiver_first(value: String) -> String {
  value.>match {
    .(s: String) { s }
  }
}

fn receiver_after_blocks(value: String) -> String {
  match {
    .(s: String) { s }
  }.<value
}
```

Prefix arguments retain their required parentheses: `r.>f(a) { body }` and `f(a) { body }.<r`.
Put a reverse splice after **all** the labelled blocks, not between the prefix
and the first block. Grouping the call, as in `((f(a) { body })).<r`, does not
change the insertion. Each call takes only one splice; parentheses do not
allow a second receiver. A following forward splice instead uses the result:
`f(a) { body }.<r.>g` passes the completed result to `g`.

The right-hand receiver is still tight. In `f(a) { body }.<r(x).>g`, it is
`r(x)`, and `g` receives the completed call's result. Write
`f(a) { body }.<(r.>g)` to insert `g`'s result instead.

A reverse splice needs explicit braces around that call's final labelled
child. This unbraced spelling is rejected for both `.<` and `.<<`:

```text
outer(a) { A } label inner(b) { B }.<r
```

Write `outer(a) { A } label { inner(b) { B } }.<r` to insert `r` into `outer`.
The diagnostic offers those braces as a fix; it does not move the receiver.
To insert into `inner` instead, write
`outer(a) { A } label { inner(b) { B }.<r }`. The formatter keeps the label
braces in both forms. Parentheses around the complete outer call do not make
the unbraced form valid.

The restriction is local to the reverse-spliced call's own final label.
Direct calls and forward splices can still use the unbraced shorthand, and
deeper ordinary chains inside an explicit outer block can use it too.

For example, these two explicit arrangements use the imported `if` macro.
Both return `value`, but the first inserts the outer condition and the second
inserts the inner condition. As with `match` above, adjust the module path to
your configured library dependency:

```kio {}
mod nested_block_splices;

import control { type Bool; macro if }

fn outer_splice[A](value: A) -> A {
  if {
    value
  } else {
    if(.f[Bool]) { value } else { value }
  }.<(.t[Bool])
}

fn inner_splice[A](value: A) -> A {
  if(.t[Bool]) {
    value
  } else {
    if { value } else { value }.<(.f[Bool])
  }
}
```

Labels cannot continue after a reverse receiver, and a reverse splice does
not reopen an already applied result such as `f { body }(x)`.

Empty UFCS lists remain errors: `f() { body }.<r` rejects. Use
`f { body }.<r` for no extra prefix argument, or `f(()) { body }.<r`
for an extra Unit. Direct `f() { body }` still passes one Unit prefix.

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

- Use `.>` when the value is the subject and the function naturally follows it: `(row.>narrow_prod)[Name]`.
- Use `.>>` when a pipeline value belongs after already-written arguments: `msg.>>string_concat(prefix)`.
- Use `.<` when you have written a function call and want to append one value: `pair(left).<right`.
- Use `.<<` when you have written a function call and want to prepend one value: `pair(right).<<left`. For a target-directed macro, its target bracket follows the completed splice: `(narrow_prod.<<value)[Target]`.

All four forms are surface syntax only. During typing, ordinary UFCS calls record normalized prefix calls, and macro UFCS calls record the same elaboration the equivalent prefix macro call would produce.

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
results. Computed callees, explicit staged calls such as `f(a)(b)`, macro
calls, and existing UFCS calls are not chain links; they may remain unchanged
inside a retained argument.

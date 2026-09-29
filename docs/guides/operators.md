# Operators

`op` gives a function a symbolic spelling. Operators such as `+` and `::`
come from declarations or explicit imports. This guide assumes familiarity
with [functions and modules](../tutorials/language.md).

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import control(if);

host type Int role(i32);
host type Bool role(bool);
host fn add(a: Int, b: Int) -> Int;

__INSERT_CODE_HERE__
-->

## Define and use an operator

With a host-provided `Int` and `add`, define a binary operator like this:

```kio {@module}
fn add_int(a: Int, b: Int) -> Int { add(a, b) }

op _ + _ { impl add_int }

fn three() -> Int { 1 + 2 }
```

Each `_` is an operand slot: `1 + 2` calls `add_int(1, 2)`. Slots supply the
function's arguments in written order. The body contains one `impl` target:
a function name, an imported path such as `math.add`, or a newtype member such
as `List.cons`. Put any argument reordering or other adaptation in a named
helper; calls and inline lambdas cannot be `impl` targets.

Declare local targets before the operator, and the operator before functions
that use its syntax. Import a target from another module through a selective
import or an explicit module alias; a slash-qualified path is not an `impl`
target.

## Choose how it chains

| Slot | Accepts |
| --- | --- |
| `_` | One operand, such as a name, literal, call, or parenthesized expression |
| `__` | An operand or a chain of this same operator |
| `___` | An operand or a chain of any one operator |

The position of `__` or `___` chooses the grouping:

| Pattern | Use | Equivalent calls |
| --- | --- | --- |
| `_ + _` | `(a + b) + c` | `add(add(a, b), c)`; parentheses required |
| `_ + __` | `a + b + c` | `add(a, add(b, c))` |
| `__ + _` | `a + b + c` | `add(add(a, b), c)` |
| `_ $ ___` | `f $ a + b` | `apply(f, add(a, b))` |
| `- __` | `- - a` | `neg(neg(a))` |
| `__ ?` | `a ? ?` | `check(check(a))` |

A pattern may contain at most one `__` or `___`; adjacent slots and longer
underscore runs are rejected. Kio has no operator precedence: write
`a + (b * c)` or `(a + b) * c`. A greedy slot admits one operator's chain,
but still needs parentheses when that chain mixes operators. Prefix unary
operators bind to their operand, so `- a + b` means `add(neg(a), b)`.

## Multi-token patterns

A pattern can contain several symbols and operands:

```kio {@module}
fn cond[A](c: Bool, t: A, e: A) -> A {
  if! c {
    t
  } else {
    e
  }
}

op _ ? _ : __ { impl cond }

fn choose(c: Bool, d: Bool) -> Int { c ? 1 : d ? 2 : 3 }
```

`choose` calls `cond(c, 1, cond(d, 2, 3))`.

For a delimited pattern, `op _ ( <| _ |> ) { impl index }` permits
`xs <| a + b |>`: the pattern's parentheses let the enclosed slot accept a
single-operator chain. They are omitted at use sites. Such groups cannot nest
or contain `___`. A recursive slot can also precede closing symbols:
`_ <| __ |>` permits `a <| b <| c |> |>`.

## Allowed symbols

Fixed operator tokens use these ASCII characters:

```text
+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | : .
```

Combine them into tokens such as `+`, `!=`, `<=`, `::`, or `<|>`.
Whitespace separates tokens: `&& ++` and `&&++` are different spellings.

- `=` must be quoted in declarations and imports: `op _ (=) _ { impl equal }`.
  Use it without quotation: `a = b`. Both `:` and `->` work directly.
- A token starting with `.` must contain at least two dots: `..` and `.+.`
  work; `.`, `.>`, and `.+` are reserved, even when quoted.
- A token may contain at most one `/`; `//` starts a comment.
- Letters, digits, Unicode symbols, and square brackets are not fixed operator
  tokens. Square-bracket delimiters use [`varop`](variadic-operators.md).

## Export and import

Use `pub op` to export an operator and make its directly named target public
too. For example, `pub op _ + __ { impl add_int }` requires `pub fn add_int`.
For `pub(path)`, the target must be visible throughout that path. A newtype
member target requires both its type or identity alias and the member to meet
the same visibility requirement.

Import the complete pattern, including its slot kinds, quotation, and grouping:
`import math(op _ + __);`. For example, the dictionary library exports `=>`
as a pair-building operator:

```kio {}
module entry_example;

import dict(op _ => _);

host type Int role(i32);

fn entry() -> Int & Int { 1 => 10 }
```

Importing the operator is sufficient to use its syntax; importing only its
function is not. Write each operator import once. Conflicting patterns in one
scope are rejected; in particular, slot changes do not let two operators
reuse the same opening pattern. Prefix and non-prefix operators can share
symbols, as with unary and binary `-`.

The [variadic-operator guide](variadic-operators.md) covers collection literals.
The [optics case study](../poc/optics.md) shows a larger operator vocabulary.
See the [operator specification](../../specs/language.md#operators) for the
complete grouping and conflict rules.

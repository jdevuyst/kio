# Operators

`op` gives a function a symbolic spelling. Operators such as `+` and `::`
come from declarations or explicit imports. This guide assumes familiarity
with [functions and modules](../tutorials/language.md).

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import control(Bool, macro if);

host type Int role(i32);
host fn add(a: Int, b: Int) -> Int;

__INSERT_CODE_HERE__
-->

## Define and use an operator

With a host-provided `Int` declared with `role(i32)` and an
`add: (Int & Int) -> Int` operation, define a binary operator like this:

```kio {@module}
fn add_int(a: Int, b: Int) -> Int { add(a, b) }

op _ + _ { impl add_int }

fn three() -> Int { 1 + 2 }
```

Each `_` is an operand slot: `1 + 2` calls `add_int(1, 2)`. Slots supply the
function's arguments in written order. The body contains one `impl` target:
a function name, an imported path such as `math.add`, or a nominal member such
as `List.cons`. Put any argument reordering or other adaptation in a named
helper; calls and inline lambdas cannot be `impl` targets.
Parentheses around the whole target are transparent: `impl ((math.add))`
selects the same function as `impl math.add`. Formatting removes that grouping.

Declare local targets before the operator, and the operator before functions
that use its syntax. Import a target from another module through a selective
import or an explicit module alias; a slash-qualified path is not an `impl`
target.

## Choose how it chains

A fixed symbol at the end of a pattern closes its expression, much like a
closing parenthesis. The result is an *atom*: it can fill an ordinary `_`
slot. A pattern ending in an operand needs explicit chain permission or
parentheses when used there. This visible endpoint rule applies to every
pattern shape.

| Slot | Accepts |
| --- | --- |
| `_` | An atom: a name, literal, call, parenthesized expression, or an operator completed by a final symbol |
| `__` | An atom or a chain of this same operator |
| `___` | An atom or an operator chain; nested slots keep their own rules |

The position of `__` or `___` chooses the grouping:

| Pattern | Use | Equivalent calls |
| --- | --- | --- |
| `_ + _` | `(a + b) + c` | `add(add(a, b), c)`; parentheses required |
| `_ + __` | `a + b + c` | `add(a, add(b, c))` |
| `__ + _` | `a + b + c` | `add(add(a, b), c)` |
| `_ $ ___` | `f $ a + b` | `apply(f, add(a, b))` |
| `- __` | `- - a` | `neg(neg(a))` |
| `_ ?` | `a ? ?` | `check(check(a))`; each completed result is an atom |

A pattern can contain several `__` and `___` slots. It must alternate single
symbol words and slots, and contain at least one of each; adjacent slots,
adjacent words, and longer underscore runs are rejected. Every slot takes a
value, not a raw type such as `Int`.

Kio has no operator precedence: with `_ + _` and `_ * _`, write
`a + (b * c)` or `(a + b) * c`. Nested operators keep their own slot rules
even inside `___`. A prefix pattern ending in an operand does not complete
an atom: `! _` needs `!(!a)` for repetition and `(!a) + b` before strict
`_ + _`. Declare `! __` when bare self-nesting is intended.

A leading `___` takes the widest completed expression before it. With
`_ + _` and `___ < _ >`, `a + b < key >` means `(a + b) < key >`.
Write `a + (b < key >)` for the other reading. If the preceding operator
also claims the following chain, neither wins: `_ $ ___` and `___ < _ >`
require `(a $ b) < key >` or `a $ (b < key >)`. Failed operand checks never
cause the parser to choose a smaller grouping.

The same conflict makes operand-ending patterns with chain slots at both
ends invalid, including `__ + __` and `___ + ___`. A `__` must add a useful
permission: it is rejected in patterns ending in a fixed word, whose results
are already atoms. It is also rejected in a later slot immediately followed
by the same word that starts a non-prefix pattern, as in `_ ? __ ? _`.
The [specification](../../specs/language.md#operators) gives the full rule.

## Multi-token patterns

A pattern can contain several symbols and operands. This example also uses a
host `Bool` with `role(bool)`, shared with or rehosted from `control.Bool`, and `if` from the
[macro library](using-libraries.md#the-macro-library); with that
dependency named `macros`, import it as `import macros/control(Bool, macro if);`:

```kio {@module}
fn cond[A](c: Bool, t: A, e: A) -> A {
  if(c) { t } else { e }
}

op _ ? _ : __ { impl cond }

fn choose(c: Bool, d: Bool) -> Int { c ? 1 : d ? 2 : 3 }
```

`choose` calls `cond(c, 1, cond(d, 2, 3))`.

For a delimited pattern, `op _ <| ___ |> { impl index }` permits
`xs <| a + b |>`: the inner `___` admits that chain. With `_ <| _ |>`,
write `xs <| (a + b) |>` instead. A closing symbol completes an atom, so
`_ <| _ |>` also permits `a <| b <| c |> |>` without a recursive slot.
The separate closing words must stay separate: `|>|>` is one different word.

A call or forward UFCS after the final symbol acts on the completed result:
`row<key>(arg)` means `(row<key>)(arg)` for `_ < _ >`.
After an operand-ending pattern, the suffix acts on its final operand:
`a + b.>f` means `a + (b.>f)`.

## Allowed symbols

Fixed operator tokens use these ASCII characters:

```text
+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | :
```

Combine them into tokens such as `+`, `!=`, `<=`, `::`, or `<|>`.
Each component is one contiguous word: `!? _` is valid, while `! ? _`
is rejected. Whitespace cannot split a word into interchangeable pieces.

- `=` must be quoted in declarations and imports: `op _ (=) _ { impl equal }`.
  Use it without quotation: `a = b`. Both `:` and `->` work directly.
- Dots belong to native syntax, including qualification and UFCS. They cannot
  occur in an operator word, even when quoted.
- A token may contain at most one `/`; `//` starts a comment.
- Letters, digits, Unicode symbols, and square brackets are not fixed operator
  tokens. Square-bracket delimiters use [`varop`](variadic-operators.md).

## Export and import

Use `pub op` to export an operator and make its directly named target public
too. For example, `pub op _ + __ { impl add_int }` requires `pub fn add_int`.
For `pub(path)`, the target must be visible throughout that path. A nominal type
member target requires both its type or identity alias and the member to meet
the same visibility requirement.

Import the complete pattern, including its slot kinds and token quotation:
`import math(op _ + __);`. For example, the dictionary library exports `=>`
as a pair-building operator:

Inside the dictionary package, imports name its local `dict` module, as
below. To consume it as a dependency named `dict`, follow the
[collection-library setup](using-libraries.md#collection-and-value-libraries)
and use `import dict/dict(op _ => _);` instead.

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

An import can change only the operator's spelling:
`import math(op _ + __ as _ ++ __);`. Write both complete grammars, with the
namespace tag only before the source. Every slot kind and whole-word boundary
stays the same. Token replacement must be
consistent and injective: repeated source tokens have one destination spelling,
and distinct source tokens remain distinct. The alias retains the provider's
implementation and installs only the destination syntax. Distinct aliases of
one source operator are allowed, subject to the ordinary scope conflicts; they
remain distinct operator identities for chaining.

The [variadic-operator guide](variadic-operators.md) covers collection literals.
The [optics case study](../poc/optics.md) shows a larger operator vocabulary.
See the [operator specification](../../specs/language.md#operators) for the
complete grouping and conflict rules.

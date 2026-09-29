# Variadic operators

`varop` defines a delimited expression containing any number of comma-separated
elements. It names ordinary functions that combine those elements from the
left or right. This guide builds on [functions and newtypes](../tutorials/language.md)
and [fixed operators](operators.md).

<!--kio {harness=collections accumulate placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(widen_sum);

host type I32 role(i32);
host fn add(a: I32, b: I32) -> I32;

__INSERT_CODE_HERE__
-->

## Define and use a collection literal

Here `nil()` constructs an empty list and `cons(head, tail)` prepends an
element. The imported `widen_sum!` elaborator constructs their sum payloads:

```kio {@collections}
rec newtype List[A] : . | (A & List(A)) { pub constructor pack; pub projector unpack }

fn nil[A]() -> List(A) { List.pack(widen_sum!((), . | (A & List(A)))) }

fn cons[A](head: A, tail: List(A)) -> List(A) {
  List.pack(widen_sum!((head, tail), . | (A & List(A))))
}

varop [* *] { foldr cons nil }

fn empty_list() -> List(I32) { [* *] }

fn three_items() -> List(I32) { [* 1, 2, 3 *] }
```

`varop [* *]` declares the opening delimiter `[*` and closing delimiter `*]`.
The clause `foldr cons nil` combines elements from the right:

```text
[* *]          → nil()
[* 1 *]        → cons(1, nil())
[* 1, 2, 3 *]  → cons(1, cons(2, cons(3, nil())))
```

The return type `List(I32)` supplies the element type for the empty literal.
Each element is an ordinary expression. Commas are the fixed separator;
leading, repeated, and trailing commas are allowed, as in tuples.

## Choose the delimiters

Delimiter tokens use these ASCII characters:

```text
+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | : . [ ]
```

The opener must contain `[` and no `]`, and cannot be bare `[`. Reverse its
characters and replace every `[` with `]` to get the closer: `[*` pairs with
`*]`, `*[` with `]*`, and `[[` with `]]`.

Write whitespace between the delimiters in declarations and imports:
`varop [* *]`, never `varop [**]`. Keep that separation in empty and nested
literals too: `[* *]` and `[* [* 1 *] *]`.
Each delimiter may contain at most one `/`; `//` starts a comment. A delimiter
starting with `.` must contain at least two dots. Letters, digits, and Unicode
symbols are excluded.

## Choose the fold mode

Write exactly one of these primary clauses:

| Clause | Meaning for elements `a, b, c` | Empty literal |
| --- | --- | --- |
| `foldl step base` | `step(step(step(base(), a), b), c)` | `base()` |
| `foldr step base` | `step(a, step(b, step(c, base())))` | `base()` |
| `foldl1 step seed` | `step(step(seed(a), b), c)` | Rejected |
| `foldr1 step seed` | `step(a, step(b, seed(c)))` | Rejected |

For `foldl`, each step receives the accumulator first; for `foldr`, it receives
the element first. The base is called once without arguments, including for an
empty literal. The `1` modes instead call a unary seed with the first or last
element. A singleton calls only the seed, with no steps. The mode chooses the
direction independently of the delimiter spelling.

## Compound elements and finalizers

An element can itself be a pair. This fixed `=>` operator builds entries;
`foldl1` starts with the first entry and adds the remaining keys and values.
The optional unary `finalize` wraps the completed totals:

```kio {@collections}
newtype Entry_totals : I32 & I32 { pub constructor pack; pub projector unpack }

fn entry(key: I32, value: I32) -> I32 & I32 { (key, value) }

op _ => _ { impl entry }

fn seed_entry(item: I32 & I32) -> I32 & I32 { item }

fn accumulate_entry((keys: I32, values: I32), (key: I32, value: I32)) -> I32 & I32 {
  (add(keys, key), add(values, value))
}

fn finish_entries(totals: I32 & I32) -> Entry_totals { Entry_totals.pack(totals) }

varop [% %] { foldl1 accumulate_entry seed_entry; finalize finish_entries }

fn totals() -> Entry_totals { [% 1 => 10, 2 => 20 %] }
```

`totals()` means
`finish_entries(accumulate_entry(seed_entry(entry(1, 10)), entry(2, 20)))`.
The seed receives one complete element, including both parts of the pair.
A singleton runs the seed and finalizer; an empty `[% %]` is rejected.

At most one `finalize` clause is allowed. It runs once after the fold, including
an empty `foldl`/`foldr` result, and may change the result type. Without it,
the literal returns the accumulator. Separate clauses with `;`; either order
is accepted, and `kio fmt` places the primary clause first.

## Export and import

Every step, base, seed, and finalizer is a function name or ordinary imported
or newtype-member path, such as `helpers.cons`. Put calls, lambdas, or argument
adaptation in a named helper. Declare local targets before the `varop`, and the
`varop` before functions using its syntax.

Use `pub varop` to export the syntax. Its directly named targets must be at
least as visible as the operator; for a newtype member, this includes both the
type or identity alias and the member. Import the delimiter pair without the
body: `import list(varop [* *]);`. The mandatory whitespace stays; commas are
implicit and do not appear in the pair. Importing a helper alone does not
import the syntax. Write each import once; two variadic operators in the same
scope cannot share an opener.

For a practical example, the [dictionary library](../../test-data/poc/dict/)
exports both a pair-building `=>` and a `[% %]` variadic operator:

```kio {}
module dictionary_example;

import dict(Compare, Dict, op _ => _, varop [% %]);

host type I32 role(i32);

fn two_entries(compare: Compare(I32)) -> Dict(I32, I32) { [% 1 => 10, 2 => 20 %](compare) }

fn no_entries(compare: Compare(I32)) -> Dict(I32, I32) { [% %](compare) }

fn replace_entry(compare: Compare(I32)) -> Dict(I32, I32) { [% 1 => 10, 1 => 20 %](compare) }
```

This library uses `foldl insert_entry empty_builder`. The literal produces a
builder function; the ordinary call `(compare)` supplies its key comparator.
Entries are inserted in source order, so the repeated key in `replace_entry`
has value `20`. The return type supplies the empty dictionary's value type.

See the [operator guide](operators.md) for fixed patterns and the
[operator specification](../../specs/language.md#operators) for the full rules.

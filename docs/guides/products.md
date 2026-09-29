# Structural products and row types

A product type `A & B` says a value carries both pieces. Label values build on that: a field such as `{name = "ada"}` has a generated nominal type `Name`, and a record-shaped value is just a product of those label types.

Products are Kio's tuple foundation. A tuple value is an element of a finite cartesian product, and Kio represents those products as right-associated binary trees: `(a, b, c)` has type `A & (B & C)`. Parentheses by themselves only group, so `(x)` is `x`, `()` is the zero-component product, and product types use `&`, not commas. This choice keeps tuple values, call arguments, function domains, UFCS insertion, and product elaborators on one structural rule.

Row-polymorphic records fall out of the same machinery. A function that needs a `Name` field and does not care what else is present writes `(Name & R)`, where `R` is an ordinary type variable.

Assumed setup: a module whose host supplies `String` and `print`, plus the labels introduced below. Snippets thread together as one growing module.

<!--kio {harness=row placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(reorder_prod);

host type String role(str);
host fn print(value: String) -> .;

__INSERT_CODE_HERE__
-->

## Labels and `&`

`labels` generates one nominal type per field. Multiple labels in one value compose with `&`:

```kio {@row}
labels Record = { name: String, note: String, ready: . };

fn build_record() -> Name & Note & Ready { {name = "ada\n", note = "first\n", ready=} }
```

`()` is the no-payload case. `{ready=}` constructs the generated `Ready` label with unit payload.

When a value already has the same name as its label, write the name once:

```kio {@row}
fn build_named(name: String, note: String) -> Name & Note { {name, note} }
```

Here `{name, note}` means `{name = name, note = note}`. `kio fmt` uses this
short form whenever a label's value is the matching local name.

A later named record can reuse one of those exact fields without redeclaring
its payload:

```kio {@row}
labels Audit = { name: _, changed: . };
```

Here `name: _` means the `Name` generated above. The marker must point to an
earlier explicit label in this module; repeating `name: String` would attempt a
second declaration and is rejected.

## Labels across modules

A `pub labels` declaration exports both the generated uppercase types and the
lowercase label syntax. The generated type name is the label spelling with the
first letter capitalized: `name` -> `Name`, `error_code` -> `Error_code`.
Import both when another module needs to name the type and construct or access
the field.

`people.kio` declares and exports the label:

<!--kio {file}
module host;

host type String role(str);
-->

```kio {file}
module people;

import host(String);

pub labels Person = { name : String };
```

`people/main.kio` imports the generated type and label and uses them:

<!--kio {harness=people_main file placeholder="__SNIPPET__"}
module people/main;

__SNIPPET__
-->

```kio {@people_main}
import host(String);
import people(Name, {name});

fn build_name(value: String) -> Name { {name = value} }

fn read_name(row: Name) -> String { row.?{name} }
```

The uppercase generated name (`Name`) is the type spelling and carries the
generated members such as `Name.get`. The braced import item (`{name}`) selects
the lowercase label syntax for construction, row access, and row update. A bare
`name` import would select an ordinary value instead. A named label alias such
as `Person` is an ordinary exported type alias; importing `Person` does not
import every generated field type or label syntax it mentions. Import each
generated type and label spelling your module uses.

Write each braced label import once. Repeating it, importing one label spelling
from two providers, or declaring it locally as well as importing it is a
name-resolution error. A scoped `pub(path) labels` declaration follows the same
subtree restriction through both selective imports and qualified forms such as
`{people.name}`.

### Giving an existing label another name

Use a nonminting forwarding declaration when another module should expose a
different spelling for the same field:

```kio {file}
module display;

import people as original;

pub type {display_name} = {original.name};
```

This adds the label spelling `display_name`, not a new `Display_name` type.
Values built with it still have the original `Name` type. A consumer imports
the label from `display` and the type from `people`:

<!--kio {harness=display_main file placeholder="__SNIPPET__"}
module display/main;

__SNIPPET__
-->

```kio {@display_main}
import display({display_name});
import host(String);
import people(Name);

fn build_name(value: String) -> Name { {display_name = value} }

fn read_name(row: Name) -> String { row.?{display_name} }
```

A forward has its own documentation and can be private, public, or scoped,
but cannot make its target accessible beyond the target's visibility. It
forwards the whole label family, including the original generic and
existential parameters, without declaring parameters of its own. The earlier
`name: _` reuse marker still requires an explicit `labels` entry in the same
module; it cannot use a forwarded label as its origin.

## Row-polymorphic parameters

A function that needs at least a `name` field writes `Name & R`:

```kio {@row}
fn greet[R](x: Name & R) -> . { print(x.?{name}) }

fn call_greet_full() -> . { let full = {name = "ada\n", note = "first\n", ready=}; greet(full) }
```

The type variable `R` means "the rest of the product." At the call site above, `R` is inferred as `Note & Ready`.

## Field access with `.?{...}`

`row.?{field}` projects a label payload by name:

```kio {@row}
fn read_name(row: Name & Note) -> String { row.?{name} }
```

Multiple fields are returned in the written order:

```kio {@row}
fn read_note_then_name(row: Name & Note) -> String & String { row.?{note, name} }
```

The result contains payloads, not label wrappers.

## Row-let with `let .({...}) = value`

Row-let destructures labels into local names:

```kio {@row}
fn read_with_row_let(row: Name & Note & Ready) -> String {
  let .({note as body, name, ready}) = row;
  body
}
```

Without `as`, the local name is the field name. With `as`, the field is projected into the chosen local name. Each row-let entry acts like an independent `.?{field}` access, so the entries can be written in the order that reads best for the local code; they do not have to follow the product's slot order. A row-let may name just the fields the continuation needs; it binds those payloads and leaves the original row value unchanged.

## Field update with `.!{...}`

`row.!{field = value}` evaluates the row once, removes the visible field if present, and prepends the replacement. If the field is absent, the same syntax adds it. With several fields, replacements are prepended in the written order.

```kio {@row}
fn replace_name(row: Name & Note) -> Name & Note { row.!{name = "grace\n"} }

fn add_note(row: Name) -> Note & Name { row.!{note = "new\n"} }

fn refresh_ready(row: Name & Note & Ready) -> Ready & Name & Note { row.!{ready=} }
```

An empty update is valid and returns the row unchanged. If a caller needs a different product order than the update naturally produces, either write an expected return type that names that order (enough when it is only a permutation of the updated row) or call `reorder_prod!` explicitly.

## Product order

`Name & Note` and `Note & Name` are distinct product types. When a caller has one order and a callee wants the other, use `reorder_prod!`:

```kio {@row}
fn wants_note_first(_x: Note & Name) -> . { () }

fn call_it(x: Name & Note) -> . { wants_note_first(x.>reorder_prod!(Note & Name)) }
```

## Where this leads

- [The optics library](../poc/optics.md) builds first-class get/set pairs on the same field-access shape.
- [Using libraries](using-libraries.md) explains how to consume the elaborator
  library that provides the structural product helpers used by these examples.

# Row-typed records with ordinary products

A row-typed record is an ordinary product whose components represent fields.
Nominal types distinguish those fields: a `Name` and a `Note` can both contain
a string without becoming interchangeable. Library macros build, select and
update the fields. Type parameters let a function accept additional fields.

This guide puts those pieces together. It assumes ordinary functions and
tuples from the [language tutorial](../tutorials/language.md#chapter-5--tuples-and-nominal-fields).
The [declaration guide](declarations.md#nominal-types) covers nominal types and
member visibility in more detail.

## Set up the macro library

Add the macro package as `macros.dep.kio` using the complete
[dependency example](using-libraries.md#the-minimal-dependency-workflow), then
run `kio dep fetch` from your package root. The dependency's package path is
`test-data/poc/macros/workdir/macros.pkg.kio`; its local name determines the
`macros/` prefix in these imports.

The following snippets build one module in order. Its host supplies the two
types and three functions below. Include the module in the package's `bridge`
when exposing it to the host, as described in
[Package files and bridges](pkg.md). The row operations themselves require no
ambient field declarations or runtime record service.

<!--kio {harness=row placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

mod kiodoc;

__INSERT_CODE_HERE__
-->

```kio {@row}
import macros/row_macros { macro record; macro record_; macro select; macro with; macro with_ }
import macros/spine_macros { macro reorder_prod }

host type String role(str);

host type Int role(i32);

host fn print(value: String);

host fn string_concat(left: String, right: String) -> String;

host fn int_to_string(value: Int) -> String;
```

## Declare fields and build a person

Each entry below declares one nominal type. `Person` is an alias for their
product, `Name & Age`; it adds no wrapper around the whole record.

```kio {@row}
type Person = { Name: String; Age: Int };

type { Note: String; Ready: . }

fn person(name: String, age: Int) -> Person {
  record { Age.mk(age); Name.mk(name) }
}
```

`Name.mk` wraps a string as a `Name`; `Name.get` projects it. Both are ordinary
functions. The block supplies constructed values, in this case an `Age` and a
`Name`. The return type supplies `record` with the target `Person`, so it
arranges those values as `Name & Age`. It evaluates the written expressions in
their original order before arranging the result.

Use `record_` when the written order should determine the result:

```kio {@row}
fn print_person(name: String, age: Int) {
  let value = record_ { Name.mk(name); Age.mk(age) };
  let .(name_text, years) = value.>select { Name.get; Age.get };
  print(name_text);
  print(int_to_string(years))
}
```

The local `value` has type `Name & Age` without a target annotation. The
selected results are the payloads, `String & Int`, rather than the wrappers.
The ordinary tuple `(Name.mk(name), Age.mk(age))` also builds that product.

| Operation | How its result is chosen |
| --- | --- |
| `record { fields }` | Requires a target from context or an explicit type argument; rearranges exactly those fields to it. |
| `record_ { fields }` | Uses the written product order. |
| `row.>with { updates }` | Updates the fields, then rearranges them to a required target. |
| `row.>with_ { updates }` | Uses the natural update order described below. |
| `row.>select { functions }` | Uses the functions' result types in selector order. |

A natural call may carry an explicit result type when that type agrees with
its result. For example, `record_((Name.mk(name), Age.mk(age)))[Person]` checks
the natural `Name & Age` result. Requesting `Age & Name` there does not make
`record_` reorder it. Use `record` when choosing a target order is the task.

## Select, transform and bind fields

A selector's input type identifies the field to use. Its result type determines
what comes back. Nominal projectors are common selectors, but any ordinary
function with the required determined input type can transform a field:

```kio {@row}
fn greeting(name: Name) -> String { string_concat("Hello, ", Name.get(name)) }

fn greeting_and_age(value: Person) -> String & Int {
  value.>select { greeting; Age.get }
}

fn age_then_name(value: Person) -> Int & String {
  let .(years, name) = value.>select { Age.get; Name.get };
  (years, name)
}
```

Each selector uses the first receiver slot of its exact input type. Selectors
may repeat, and the result follows selector order. The receiver is evaluated
once, then all selector expressions are evaluated in order, then the selected
functions are called in order. An expression that creates a selector is
evaluated once too.

## Update and add fields

Updates are constructed field values. `with_` removes the first receiver slot
of each exact update type, if present, then puts all updates first in written
order. Untouched slots follow in their original order:

```kio {@row}
fn update_person(value: Person, age: Int, note: String) -> Age & Note & Name {
  value.>with_ { Age.mk(age); Note.mk(note) }
}

fn update_in_person_order(value: Person, age: Int, note: String) -> Name & Age & Note {
  value.>with { Age.mk(age); Note.mk(note) }
}
```

Both calls replace `Age`, add `Note` and retain `Name`. The first produces
`Age & Note & Name`. The second uses its return type to request `Name & Age &
Note`. That target must be an exact product permutation: it cannot discard a
field, invent a value or convert a payload.

The receiver runs once before the update expressions, which run once in
written order. Result permutation changes no effects. Neither operation
mutates the original record; the returned product contains the updated values.

## Use fields across modules

To export a bundle from a module named `people`, declare
`pub type Person = { Name: String; Age: Int; };` there. With no entry body, the
field types and their `mk` and `get` members inherit that public visibility.
A consumer writes `import people {type Person; type Name; type Age}` to name the bundle and
use the field members. Importing `Person` alone does not also introduce `Name`
and `Age` as local names. A qualified import, `import people as p;`, provides
`p.Person`, `p.Name.mk` and `p.Name.get` instead.

Reuse an existing field identity by naming its type in another product; do not
redeclare it. For example:

```kio {@row}
type Documented_person = Name & Age & Note;

type Display_name = Name;

fn display_name(name: String) -> Name { Display_name.mk(name) }
```

`Display_name` is an identity alias, so it uses `Name`'s members and creates a
`Name`. A new nominal declaration would create a distinct field. A written
member body controls construction and projection visibility independently;
see [Visibility](declarations.md#visibility). A macro does not bypass a private
projector or constructor.

## Row polymorphism

A function can require a leading `Name` while leaving the remaining product
generic:

```kio {@row}
fn greet[R](value: Name & R) {
  print(value.>select { Name.get })
}

fn greet_person(value: Person) { greet(value) }

fn read_age[R](value: Age & R) -> Int {
  value.>select { Age.get }
}

fn age_of_person(value: Person) -> Int { read_age((value.>reorder_prod)[Age & Name]) }
```

`greet_person` supplies `R = Age`. `read_age` requires the age first, so its
caller explicitly rearranges `Person`. There is no implicit reordering at an
ordinary function call.

The tail `R` is an ordinary type parameter, not an unordered set of fields.
Within `greet`, an opaque `R` cannot be searched for an `Age` merely because
one particular caller supplies one. State a needed field in the parameter's
known product shape, or arrange the product before calling the helper.

## Generic fields match by their complete type

A field owns its own parameters. `Field[Int]` and `Field[String]` below are
different field types, even though they share the nominal declaration:

```kio {@row}
type { Field[T]: T }

fn add_text(value: Field[Int], text: String) -> Field[String] & Field[Int] {
  value.>with_ { Field.mk[String](text) }
}

fn generic_payload[T](value: Field[T] & Note) -> T {
  value.>select { Field.get[T] }
}
```

The update adds `Field[String]`; it does not replace `Field[Int]`. Replacement
requires the same fully applied type. A bound generic `T` is enough to identify
`Field[T]`; it need not be a concrete host type. `Field.get[T]` explicitly
specializes the projector so the selector's input is determined. Do not rely
on searching the receiver to choose an unresolved selector parameter.

## Product order and grouping

A product type `A & B` carries both pieces. Products associate to the right:
`(a, b, c)` has type `A & (B & C)`. Parentheses group, so `(x)` is `x`;
`()` has the Unit type `.`. `Name & Age` and `Age & Name` are different types.

The row macros follow the product's right-hand chain, called its *spine*.
`(Name & Age) & Note` has a compound first slot and a `Note` slot, whereas
`Name & Age & Note` has three slots. A selector accepting `Name & Age` selects
one exact compound slot; it does not collect adjacent `Name` and `Age` slots.
A nominal wrapper keeps its entire payload in one slot regardless of its
internal product structure. A block is ordinary product construction, not a
separate container that remembers where its entries began.

## Empty packets and repeated types

| Case | Result |
| --- | --- |
| `record_ {}` | Unit `()`. |
| `value.>with_ {}` | The evaluated receiver, unchanged. |
| `value.>select {}` | Evaluates the receiver and returns `()`. |
| Two source slots of the same exact type | Both are retained; a selector uses the first. |
| One update matching repeated receiver slots | Replaces only the first match; later copies remain. |
| Two updates of the same exact type in one block | Rejected. |
| Repeating a selector | Returns a result for every occurrence. |
| `Ready.mk(())` | One nominal field, despite its Unit payload. |

A root Unit is an empty packet. Unit inside a nonempty product, such as
`Name & .`, remains a slot. Transparent aliases preserve type identity and
therefore match the same slots. For an existential nominal, the projector
keeps its continuation interface; selecting it does not let the hidden type
escape. See [existential nominal types](declarations.md#existential-nominal-types).

The [macro case study](../poc/macros.md) gives the library's laws and structural
operations. [Dependency injection](dependency-injection.md) applies this record
pattern to capability bundles, and [sums](sums.md) explains nominal alternatives.

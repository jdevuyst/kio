# The Kio language

Start here when you want to read and write ordinary Kio source. The chapters build a working model of the language one construct at a time: packages, modules, functions, host-backed values, data shapes, matching, polymorphism, UFCS, and macro calls.

Read straight through if you are new to Kio, or skip to the chapter that matches what you need. The goal is practical familiarity: enough to read modules, write simple programs, and recognize common surface forms. Deeper treatment of individual features, such as operator definitions, UFCS, macros, and optics, lives in [docs/guides/](../guides/).

<!-- Snippets here illustrate Kio at every level: package files
(variant=package), module bodies
(via a harness), and expression-shaped fragments (a block-position
harness wraps each in `pub fn body() { ... }`). Each
chapter's harness picks up where the previous one left off. -->

<!-- Block-position harness for expression-shaped fragments: each
snippet body becomes the contents of a fresh fn. Same host
capabilities as the module-body harness below. -->
<!--kio {harness=block_expr placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

import match {macro match}
import control {type Bool; macro if; macro scope}
import spine_macros {macro narrow_prod; macro one_prod; macro one_sum; macro widen_sum}

host type Int role(i32);
host type String role(str);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String);

pub fn body() {
  __INSERT_CODE_HERE__
}
-->

<!-- Top-level harness for module-body fragments — fn / type / literal /
nominal / `pub fn main` declarations. -->
<!--kio {harness=tutorial_module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

import match {macro match}
import control {type Bool; macro if; macro scope}
import spine_macros {macro narrow_prod; macro one_prod; macro one_sum; macro widen_sum}

host type Int role(i32);
host type String role(str);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String);

__INSERT_CODE_HERE__
-->

## Chapter 1 — Your first Kio program

A Kio program is a **package** — a directory of files. The smallest interesting package has two kinds of files: a *package file* declaring the package's contract with its host (and, in an optional leading `build { ... }` block, naming the package's compilation targets), and one or more *modules* holding the actual code.

The canonical "hello, world" is one of each.

The package file lives at the package root. Its filename stem (`hello`) is the package's name. The optional `build { ... }` block comes first, naming a compilation target; then a `bridge { ... }` block lists the modules whose public surface forms the package's contract with its host.

```kio {file}
// hello.pkg.kio
pkg hello;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/"
  }
}

bridge {
  hello;
  hello/**
}
```

The module declares the host capabilities it needs as `host` declarations, and
the submodule at `hello/main.kio` reaches them with an `import`.

```kio {file}
// hello.kio
mod hello;

host type String role(str);

host fn print(p0: String);
```

`host` marks types and functions supplied by the program running this package.
`String` is a name this module gives a host-supplied type, not a builtin type.
`role(str)` allows string literals to have this type. A concrete expected type,
such as `print`'s parameter, determines the carrier of an unannotated string.
The host also supplies `print`, which accepts values of that type.

```kio {file}
// hello/main.kio
mod hello/main;

import hello {type String; fn print}

pub fn main() { print("hello\n") }
```

The imported `String` and `print` retain their original type identities.
The call's parameter type determines the string literal's carrier. See
[literal types and local inference](../guides/typechecking.md#literal-types-and-local-inference)
for explicit annotations and unresolved literals.

<!--kio {harness=hello_check file placeholder="__CHECK__"}
mod hello/doc_check;

import hello/main {fn main}

__CHECK__
-->

<!--kio {@hello_check}
fn check_main() { main() }
-->

Build the package:

```sh
kio build
```

The output is an ES module at `out/js/hello.js`. A JavaScript host imports its `createHello` factory (branded from the package namespace `hello`), supplies the host record (the `print` function the package's `host fn` declared), and invokes the package's `main`. Running the result prints:

```text
hello
```

That's a complete Kio program. The shape — a package file (with its `build { ... }` block) and modules — is the same in every Kio package regardless of size. The package file is the **contract**: which modules' public surface the host sees, splitting into what the host owes the package (the `host` items) and what the package owes the host (the other `pub` items). Everything else fills it in. For a closer look at package files and the bridge block, see [Package files and bridges](../guides/pkg.md).

### How later chapters read

From here on, the surrounding scaffolding (package file declarations, module header, and host declarations) is implicit. Each chapter focuses on one construct at a time, and the snippets show only the lines that matter.

When you see a snippet like

```kio {@block_expr}
print("hello again\n")
```

read it as *the body of `pub fn main() { ... }` in a module whose host supplies `String`, `print`, and the other capabilities the chapter assumes* — the same shape as the hello-world above, with those `host` items exposed by the package's `bridge` block when they cross the package boundary.

## Chapter 2 — Inside a module

The hello-world's `hello/main.kio` is a **module**. A module is a `.kio` source file with a `mod` declaration at the top, optional `import` statements pulling names into scope, and a sequence of definitions — types, functions, operators. Modules group definitions; the *package* groups modules.

The `mod hello/main;` line says "this file is the module at path `hello/main`." The path mirrors the directory layout: `hello/main.kio` is the module `hello/main`; `hello/util/string.kio` would be `hello/util/string`.

### Import statements

`import` brings names into the module's scope. The common shape pulls items from another module in the same package:

`import some/module {type X}` lists one or more semicolon-separated items. Every item specifies its kind: `fn`, `type`, `lit`, `macro`, `op`, `op_macro`, or `op_var`. A `macro name` item selects a macro, for example `import match {macro match}`. Importing an ordinary nominal type also makes its accessible members available: `import people {type Name}` lets a consumer use `Name.mk` and `Name.get` when those members are public. Host capabilities are no exception: `host type` and `host fn` declarations are ordinary public items of the module that declares them, so the hello-world brings in `String` and `print` with `import hello {type String; fn print}`.

When another module is mostly used as a source of names, prefer the qualified alias form where that import surface supports it: `import some/module as m;` or `import utils/list_ops as l;`. The call site then keeps the origin visible, as in `m.helper(...)`. The alias also reaches exported types, type constructors, and nominal members: `m.Shape` names a type, `m.Stack[Item]` applies a type constructor, and `m.Stack.get` reaches a public nominal member. These remain qualified names; the alias does not add them to unqualified scope. Use selective imports when the bare name is the point of the example, part of the module's public vocabulary, or required by syntax. The special compile-time and core imports are the exceptions: `import __comptime__;` and `import __intrinsics__;` are block imports with no alias form. Ordinary programs rarely need `__intrinsics__`; use tuples, nominal types, and imported library macros such as `match` and the structural palette for normal product and sum code.

To choose a local spelling, use a selective alias:
`import hello {type String as Text; fn print as write}`. The destination keeps the
source's type-shaped or value-shaped identifier class. This selects the same
declaration, with the same type and visibility; it does not create a wrapper or
re-export it. Multiple distinct local aliases are allowed.

An operator is selected by its complete tagged grammar, for example
`import arithmetic {op _ + __}` or `import list {op_var [* *]}`.
The import supplies the syntax needed to parse this module; semantic checking
then requires an exact exported match in the named provider. Importing the
underlying function alone does not import its operator. See the
[operator guide](../guides/operators.md) for fixed patterns and the
[variadic-operator guide](../guides/variadic-operators.md) for collection syntax.

Selective imports have a nonempty braced list even for one name. A long list
keeps `{` on the module-path line and places each item on its own indented
line. Each local name or module alias is introduced once: repeating an import
is an error, even from the same provider. The two fixed builtin block imports
are idempotent. Macros and literal aliases require selective imports; qualified
expression references expose functions and types.

### pub and fn

`fn name(args) -> Return { body }` defines a function. `pub` before `fn` makes the function visible to other modules — and, when the module is bridged, to the package surface, which is how the hello-world's `main` reaches the host. Without `pub`, the function is module-private.

For a named `fn` or `host fn`, leaving out the return annotation fixes its result to unit (`.`). An ordinary function's body must produce unit rather than determine a different result type.

Same idea for `type` and `lit`: `pub` makes them visible; declarations without `pub` are module-private. The package's `bridge` block exposes only the `pub` items of the modules it matches; non-`pub` items stay internal regardless.

### Definitions are read top-to-bottom

A module's declarations are processed in order, and each one sees only the names declared *above* it (plus whatever `import` brought in). There is no forward reference within a file: a `fn` that calls a helper must come *after* that helper's definition, and a `fn` cannot call itself by name. Order helpers before their callers. Host declarations follow the same rule: declare a `host type` before any signature that mentions it, and a `host fn` before code or a declaration that names it.

Recursive scope is explicit. `rec type { ... }` gives a nominal declaration
its own head, and `rec { ... }` groups mutually recursive declarations. See
[recursive nominal types](../guides/declarations.md#recursive-nominal-types).

### Module dependencies cannot cycle

Every explicit `import` adds a dependency edge between module files. Those edges
must be acyclic: if module `a` imports from `b`, then `b` cannot directly or
transitively import from `a`. A type-only import and an import of a `host` item
count exactly like a value import; there is no exemption based on what name the
`import` selects. The package file's `bridge` block is not a module-body import and
does not add an edge.

This also means mutually recursive types cannot be split across two modules,
because each half would have to import the other. Put the complete mutual
component in one bare `rec { ... }` type group in one module, and let consumers
import that module in one direction.

## Chapter 3 — Values and atomic types

Kio has no built-in numeric or string types. Every atomic value — integers, floats, strings, booleans — is typed by a `host type` in scope carrying a `role` annotation. A module declares the host types and functions it needs; the package's `bridge` block exposes a bridged module's host declarations to the host as requirements.

Recall `hello.kio`:

```kio {variant=module}
// hello.kio
mod hello;

host type String role(str);

host fn print(p0: String);
```

`host` marks types and functions supplied by the program running this package.
`String` is a name this module gives a host-supplied type, not a builtin type.
`role(str)` allows string literals to have this type. A concrete expected
parameter or return type can determine an unannotated string's carrier. The
host also supplies `print`, which accepts values of that type.

`host type String role(str);` says "the host supplies a type, this module will call it `String`, and string literals checked against it produce values of that type." `role(str)` says that this declared type accepts Kio string literals; the host binding supplies its representation.

Role-bearing host types are atomic and therefore have no type parameters. A
roleless host type may still be parameterized, such as `host type Array[A];`;
`host type Array[A] role(i32);` is rejected.

Other roles admit numeric literals. A concrete return type can determine
the carrier of an ordinary unannotated literal:

```kio {variant=module}
// arith.kio
mod arith;

host type Int role(i32);

host type String role(str);

host type Bool role(bool);

host fn add(p0: Int, p1: Int) -> Int;

host fn int_to_string(p0: Int) -> String;

host fn print(p0: String);
```

The package file admits that module's surface by listing it in `bridge`:

```kio {variant=package}
// arith.pkg.kio
pkg arith;

bridge {
  arith;
  arith/**
}
```

Inside `arith`, the concrete function signatures determine the numeric and
string literal carriers. Other modules can import these host types normally;
merely importing them never supplies missing contextual evidence.

Write a negative number with `-` touching the first digit, such as `-40` or
`-3.14`. Kio reads that spelling as one literal where a new expression can
begin, including after `=`, `(`, `,`, or another operator (`1 + -2`). After a
completed expression, `-` starts an operator instead, so `1-2` and `value-1`
are subtraction shapes. Whitespace matters: at the start of an expression,
`- 40` is a prefix `-` operator applied to `40`, not a negative literal, and
therefore requires that operator to be declared.

Float roles work the same way. A decimal literal needs a float-shaped role type,
and an explicit annotation is the clearest spelling when the surrounding context
does not already force the type:

```kio {variant=module}
mod temps;

host type F64 role(f64);

host fn add_f64(p0: F64, p1: F64) -> F64;

pub fn adjusted() -> F64 { add_f64(19.5[F64], 0.25[F64]) }
```

Kio checks integer literals against their selected role's range: `i8`
admits −128 through 127, for example, and `u8` admits 0 through 255.
Leading zeros do not mean octal, and integer `-0` is zero. Floating literals
round directly to IEEE binary32 for `f32` or binary64 for `f64`, using
nearest-even rounding. Overflow is a type error; subnormals and negative
zero are preserved, including when a tiny value underflows to zero. These
rules determine literal values; the host still supplies arithmetic.

An explicit literal annotation such as `42[Int]` selects its carrier.
Otherwise, a concrete expected return, argument or binding type can determine
it within ordinary local inference. If the carrier remains unknown at that
boundary, annotate the literal. Merely importing a role-bearing type does not
select it, and there is no fallback or ambient search among visible types.

There are four categories: integer (all signed and unsigned widths), float
(`f32`/`f64`), Boolean and string. Multiple types can share a category or role.
A transparent alias inherits its normalized host type's literal admission;
it introduces no automatic selection. Ordinary untyped `let` initializers
finish before later statements, so a later call cannot choose their type.

Floating-point syntax requires a decimal point: `42.0`, `42.0e3` and
`42.0e-3` are floats. Integers have no exponent notation, so `42e0`, `42E+3`,
`42e-3` and `42e-0` all reject. Neither an expected float type nor
`42[F64]` turns integer syntax into a float: write `42.0` or `42.0[F64]`.
For a generic function requiring two values of the same type,
`same(42, 42.0)` and `same(42.0, 42)` both reject.
See [literal types and local inference](../guides/typechecking.md#literal-types-and-local-inference)
for aliases, source scope, conflicts and editor repairs.

Operations on these types — `add`, `mul`, `int_to_string`, `print` — are `host fn`s. Kio does not auto-include a standard library: the host supplies whatever the bridged modules' `host` declarations ask for, and reusable Kio code arrives through ordinary modules and dependencies. The compiler provides the core and compile-time pseudo-modules named in Chapter 2, but libraries such as the structural macro palette are user Kio code that a package imports explicitly.

For the rest of the tutorial, assume the module declares `Int`, `String`, `Bool`, the obvious numeric ops, and `print` / `int_to_string` as `host` items. Each chapter's snippets sit inside `arith/main.kio` or its equivalent.

## Chapter 4 — Functions

A function is a `fn` at module level (or a `fn` expression inline).

```kio {@tutorial_module}
fn double(n: Int) -> Int { add(n, n) }
```

Parameters are `name: Type` pairs; a written return type follows `->`. An omitted return type on a named `fn` or `host fn` means unit (`.`). The `double` example writes `-> Int` because it does not return unit. A block is one or more statements separated by `;`, ending in a trailing expression whose value is the block's value.

### let bindings

`let name = expr;` binds a name to a value for the rest of the block.

```kio {@tutorial_module}
fn quadruple(n: Int) -> Int { let twice = add(n, n); add(twice, twice) }
```

Write `let .(name: Type) = expr;` when the RHS needs that local expected type:

```kio {@tutorial_module}
fn add_one(n: Int) -> Int {
  let .(inc: Int -> Int) = .(x) { add(x, 1) };
  inc(n)
}
```

`let` may shadow an outer binding of the same name; the inner binding wins for the rest of the block.

### Expression statements

An expression before another clause runs for its side effect and must have
type `.` (unit) — most host I/O functions already return it. A semicolon
separates clauses; it does not discard a value.

```kio {@tutorial_module}
fn greet_twice() { print("hi\n"); print("hi again\n") }
```

The first `print` is an expression statement; the second is the block's final
expression. The final expression supplies the block's value even when followed
by `;`. To discard a non-unit value deliberately, use `let _ = e;`.

### Inline functions

A function value can appear inline as `.(args) { body }` — a lambda.

```kio {@tutorial_module}
fn apply(f: Int -> Int, x: Int) -> Int { f(x) }

fn immediate_identity() -> Int {
  .(n) { n }(41[Int])
}

pub fn main() {
  let y = apply(.(n) { add(n, 1) }, 41);
  print(int_to_string(y))
}
```

A lambda's parameter types can be elided when the expected type is fixed by context — here, by `apply`'s signature. They can also be written explicitly (`.(n: Int) { add(n, 1) }`) for emphasis. Unlike a module-level `fn`, a lambda without a written return type takes its result from its body and surrounding expected function type; omission does not mean unit.

A function type returning a product or sum groups that result:
`Int -> (Int & Int)` or `Int -> (Int | String)`. A function or lambda's own
return annotation can stay bare, as in `fn pair(n: Int) -> Int & Int`.
Returning another function uses the type-expression rule again:
`fn make_pair() -> Int -> (Int & Int)`. Arrow-only chains such as
`Int -> Int -> Int` need no extra parentheses.

Checked annotations may mix concrete structure with local `_` placeholders,
but a placeholder cannot sit beneath a `forall` written inside that same
annotation. `.[A](x: _) -> _ { x }` remains valid because `[A]` is a header
binder and both annotations are whole slots. By contrast,
`.(f: [A] A -> _) { ... }` is rejected: the placeholder is inside `f`'s own
polymorphic annotation. Write the concrete result beneath that binder, or make
the whole `f` annotation `_` so the surrounding function slot supplies it.

An immediate call can also supply a whole missing parameter type:
`immediate_identity` gives `n` the independently known type of `41[Int]`. The rule
is local to that call. Binding `.(n) { n }` with an untyped `let` and calling
the binding later does not send type information backward across the `let`;
give the binding a concrete function-type annotation when it must be named
first.

### Conditionals

`import macros/control {type Bool; macro if}` selects the library's Boolean type
and conditional macro when the dependency is named `macros`; see
[macro-library adoption](../guides/using-libraries.md#the-macro-library).
Importing `if` provides a macro call with a parenthesized condition and two
blocks. Every nonempty written macro prefix uses ordinary argument-list parentheses.
Both branches produce one common result type:

```kio {@tutorial_module}
fn choose(condition: Bool, when_true: Int, when_false: Int) -> Int {
  if(condition) { when_true } else { when_false }
}
```

The reference library's `if` uses its declared `control.Bool` host type.
Consumers share that type by importing it, or rehost the control module to
an existing Boolean-capability module. The generated intrinsic checks this
concrete Boolean specialization. Both arms are
blocks. When the surrounding position supplies an expected result type, it
checks both arms. Without that context, the two branch results determine their
common type together: a result that is already clear in either branch can
determine a generic result in the other, and swapping the branches does not
change the inferred type. If neither branch nor the surrounding position
determines the result, add an ordinary result annotation.
Incompatible branch types are an error rather than an automatic sum; construct
the sum explicitly when that is the intended result. At runtime, only the
selected branch runs.

### Scoped expressions

The imported `scope` macro turns an ordinary scoped block into an expression. It is useful in
an argument, conditional arm, or `equiv` arm where local statements need a
wrapper:

```kio {@tutorial_module}
fn increment_in_expression(value: Int) -> Int {
  scope { let next = add(value, 1); next }
}
```

The body admits ordinary `let`, expression-statement, and trailing-expression
rules. `do(bind) { let x <- ...; result }` is a distinct, bind-based form covered
in [Higher-kinded types](../guides/higher-kinded-types.md#monadic-do).

### Placeholder lambdas

A placeholder lambda is compact partial-application syntax. Its identifier
stem appears between adjacent dots and names its parameter references:

```kio {@tutorial_module}
fn apply_int(f: Int -> Int, value: Int) -> Int { f(value) }

fn increment_placeholder(value: Int) -> Int {
  apply_int(.x. { add(x1, 1) }, value)
}
```

`.x. { add(x1, 1) }` lowers to a one-parameter lambda. Indexed references may
repeat (`.x. { pair(x1, x1) }`) or reorder (`.x. { pair(x2, x1) }`); each
reference uses a positive index without a leading zero, such as `x1` or `x2`.
The highest index determines the lambda's arity. Other ordinary value-name
stems ending in a letter, such as `.arg.`, work too; each nested placeholder
lambda owns a fresh stem. Parameter types come from the surrounding expected
function type, just as they do for an unannotated `.(x) { ... }` lambda.

## Chapter 5 — Tuples and nominal fields

Tuples combine values in order. Nominal field types give their components
distinct identities, so a product can serve as a row-typed record.

### Tuples

`(a, b)` is a 2-tuple value; its type is `(A & B)` — the `&` reads as "and." Product types model finite cartesian products, and tuple values are elements of those products. Kio does not have a separate nominal tuple family. It also does not pretend every parenthesization is the same syntax tree: Kio chooses right-associated binary products as the canonical representation. `()` is the zero-item product value, `(a)` is just grouping around `a`, and `(a, b, c)` means `(a, (b, c))`.

```kio {@tutorial_module}
fn make_pair(s: String) -> String & String { (s, "world\n") }

pub fn main() {
  let pair = make_pair("hello, ");
  let .(first: String, second: String) = pair;
  print(first);
  print(second)
}
```

A destructuring `let` binds the components directly. Tuples extend to any arity: `(a, b, c)`, `(a, b, c, d)`. The corresponding type uses `&` between components.

Tuple values compose with call arguments. If a function expects `(A & (B & C))`, these calls pass the same product shape: `f(a, b, c)`, `f(a, (b, c))`, and `f((a, b, c))`. Grouping still matters on the left: `f((a, b), c)` passes `(A & B)` as the first argument, which is a different shape.

The same destructuring shape can appear in a function parameter:

```kio {@tutorial_module}
fn show_pair_param((first: String, second: String)) { print(first); print(second) }
```

Parameter patterns and destructuring lets bind the product once and project the named components directly. The pattern shape extends to nesting (`((a: A, b: B), c: C)`), wildcards (`(_: A, b: B)` — keeps the slot's type but doesn't name it), and an "as-pattern" form (`name: (a: A, b: B)`) that binds the whole product alongside its components.

For a short local use, a numbered binding names those slots with a shared stem:

```kio {@tutorial_module}
fn print_pair(first: String, second: String) {
  let .part. = (first, second);
  print(part1);
  print(part2)
}
```

The highest referenced index sets the number of slots, just as in a placeholder
lambda. Referencing only `part1` binds the whole value; referencing `part2`
creates two slots. The last slot receives any remaining product tail. Stems
end in a letter and indices start at one, without leading zeroes.

A later `let .part.` starts a new family after its initializer. Other stems
stay available: `let .left. = a; let .right. = b; (left1, right1)` uses both.
Explicit lambdas can refer to the family, subject to their parameter names;
a nested placeholder lambda owns its own references. Sequence blocks use the
matching `let .part. <- action;` spelling for an action's payload. Each family
must have a reference in the remaining block. Use an ordinary named binding
when a descriptive name makes the code clearer.

### Nominal fields

<!--kio {harness=tutorial_fields_block placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

import macros/row_macros {macro record_; macro select}

host type String role(str);
host fn print(p0: String);

type Greeting = { Hello: String; Goodbye: String; };

pub fn body() {
  __INSERT_CODE_HERE__
}
-->

A nominal entry introduces a distinct type around a payload. A named group
can also name the product of its entries:

```kio {@tutorial_module}
type Greeting = { Hello: String; Goodbye: String };
```

`Hello` and `Goodbye` are different types even though both wrap `String`.
Their `mk` members construct values and their `get` members project payloads:

```kio {@tutorial_fields_block}
let hi = Hello.mk("hi\n");
print(Hello.get(hi))
```

The macro library's `record_` builds a product in written order, and `select`
applies projectors to matching fields. Add the dependency as described in
[Using libraries](../guides/using-libraries.md#the-minimal-dependency-workflow).
With the dependency named `macros`, import them using
`import macros/row_macros {macro record_; macro select}`:

```kio {@tutorial_fields_block}
let pair = record_ { Hello.mk("hi\n"); Goodbye.mk("bye\n") };
let .(hello, goodbye) = pair.>select { Hello.get; Goodbye.get };
print(hello);
print(goodbye)
```

The inferred type of `pair` is `Hello & Goodbye`. Use `record` instead when
a target type should choose the product order. These are ordinary products
and nominal types. The complete workflow, including row-polymorphic functions
and updates, is covered in
[Row-typed records with ordinary products](../guides/products.md).

## Chapter 6 — Sums and pattern matching

Where `&` ("and") composes types into products, `|` ("or") composes types into **sums**.

```kio {@tutorial_module}
fn classify(v: String | Int) {
  let _r =
    match(v) {
      .(s: String) { print(s) };
      .(n: Int) { print(int_to_string(n)) }
    };
  ()
}
```

`(String | Int)` is the type of a value that is *either* a `String` *or* an `Int`. `match` takes one **clause** per arm of the sum. Each `.(name: Type) { body }` matches one branch; the body computes the result.

This is a deliberate preview of macros. `match` is imported library code, not a built-in keyword. The tutorial harness imports it for the snippets; in a real package, you depend on the module that defines `match` and bring it into scope with `import match {macro match}`. Chapter 9 comes back to the import model.

Every clause body must produce the same result type. Both arms above produce the unit value, so the `match` expression has type `.`. We bind it to `_r` only to sequence the printing before returning `()`.

### Constructing a sum value

How do you build a `(String | Int)` value from a `String`? With another preview macro, `widen_sum`, which injects a value into a target sum that contains its type as an arm:

```kio {@tutorial_module}
fn classify_again(v: String | Int) {
  let _r =
    match(v) {
      .(s: String) { print(s) };
      .(n: Int) { print(int_to_string(n)) }
    };
  ()
}

pub fn main() { let v = widen_sum("matched right\n"[String])[String | Int]; classify_again(v) }
```

`widen_sum("matched right\n"[String])[String | Int]` says "put this `String` into this target sum." Like `match`, `widen_sum` is imported library code; the tutorial harness imports it from `spine_macros`. The macro walks the target's spine for the arm a `String` fits (here, the left arm) and produces the sum value — the walk is scoped to the target type, not to what other types are in scope.

## Chapter 7 — Polymorphism

A `fn` whose type parameters appear in `[brackets]` is **polymorphic** — it works for any type substituted for the parameter.

Type-parameter names are local to their function. A function declared with `[Token]` can take a caller's unrelated type named `Token`: the type argument refers to the caller's type, not to the callee's parameter. Renaming that parameter to `[A]` throughout the function leaves its callers unchanged.

A nested lambda can declare the same type-parameter name as its enclosing
function. Inside the lambda, that name refers to the lambda's parameter;
outside it, the enclosing parameter is still in scope:

```kio {@tutorial_module}
fn identity[A](value: A) -> A { value }

fn shadow[A](value: A) -> A {
  let inner = .[A](other: A) -> A { identity[A](other) };
  inner[A](value)
}
```

Here `identity[A](other)` uses the inner `A`. The call `inner[A](value)`
supplies the outer `A`, so `shadow` returns the same type it received.

<!-- A poly-aware harness: declares `id` and a few call-site
contexts so the expression-shaped fragments below can call them. -->

<!--kio {harness=tutorial_poly_block placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String);

fn id[A](x: A) -> A { x }

fn pair[A][B](x: A, y: B) -> (A & B) { (x, y) }

pub fn body() {
  __INSERT_CODE_HERE__
}
-->

```kio {@tutorial_module}
fn id[A](x: A) -> A { x }
```

`id` takes a type argument `A` and a value `x` of that type, and returns `x`. Most call sites elide the type argument; the typer infers it from the value-arg's type:

```kio {@tutorial_poly_block}
let _i = id(42[Int]);
// A inferred as Int
let _s = id("hi"[String]);
// A inferred as String
()
```

The literal annotations give these arguments concrete carriers; without a
concrete context, `id(42)` alone cannot choose one. `id` still infers its type
argument from the resulting value type.

Each type-argument slot in the current universal/function layer has three
spellings. Write every type explicitly, put `_` in only the slots the typer
should solve, or elide that layer's leading run:

```kio {@tutorial_poly_block}
let _explicit = pair[String][Int]("left", 1);
let _partial = pair[String][_]("left", 1[Int]);
let _elided = pair("left"[String], 1[Int]);
()
```

Square brackets contain only types or whole-slot `_` placeholders; ordinary
parentheses contain values. Written type arguments are positional within the
current binder run. `pair[String][Int]` supplies the two types. Each bracket contains exactly
one type; a comma-separated type list is rejected, at both calls and binders. In the partial call, `String` fills `[A]` and
`_` asks inference to solve `[B]` from `1[Int]`. Elision omits that leading binder
run; it is not shorthand for writing a variable number of underscores among
value arguments. Consuming a value layer may expose a later binder run, which
the same rule handles as the next layer of the call.

Bare polymorphic values stay polymorphic. When a surrounding function needs a
particular instance as a value, write `_` as the type-only application:

```kio {@tutorial_poly_block}
let .(_instantiated_function: Int -> Int) = id[_];
()
```

The binding's expected function type solves `_` as `Int`, so `id[_]` is the
residual function `Int -> Int`. `id()` means something different: its syntactically empty direct
call writes one Unit value, so it calls `id` at the Unit type and produces
`()`. Written `id(())` does the same thing because `()` is only a value.
`id[.]` explicitly selects the Unit type but writes no value, so it remains the
residual function `. -> .`; write `id[.](())` to apply it. Likewise,
`id[String]` retains `String -> String`. An expected function type may solve a
written `_`, but that does not apply the function's value packet.

A type binder can also appear after a value layer. In this reference shape,
the expected return type fixes the returned binder:

```text
host fn produce(value: Seed) -> [Result] Result;
fn witness(seed: Seed) -> Witness { produce(seed) }
```

After `produce(seed)` consumes `Seed`, the declared `Witness` return type fixes
`Result`. Without that expected result, the returned polymorphic value remains
polymorphic.

A bracket group performs only type application; empty `id[]` is rejected.
For `id` above, `id[String]` has type `String -> String`, and `id[.]` has type
`. -> .`; write `id[.](())` to apply the latter function to Unit. A direct
empty call is different: `id()` omits `A`, writes one Unit value, infers
`A = .`, and produces `()`, just like `id(())`.

For a function `nil : [A] . -> List[A]`, `nil[A]` likewise retains
`. -> List[A]`, while `nil[A]()`, `nil[A](())`, and `().>nil[A]` apply the Unit
argument. The shorter direct spelling `nil()` works only where the surrounding
expected type determines `A`, and lowers with both arguments explicit. A
receiver-only UFCS call omits its list: `x.>f` means `f(x)`. The spelling
`x.>f()` is rejected; use `x.>f(())` when the call passes Unit in addition to
the receiver.

Elide type arguments when value types or a concrete expected result determine
them. When a local RHS needs a checked position, put the annotation on the
local binder:
`let .(f: Int -> Int) = .(x) { x };`.

Multi-parameter polymorphic functions work the same way:

```kio {@tutorial_module}
fn pair[A][B](x: A, y: B) -> A & B { (x, y) }
```

`pair` infers both `A` and `B` from the value-arg types. The tuple spelling is equivalent to the comma-separated spelling because call arguments and tuple values use the same right-associated binary-product shape:

```kio {@tutorial_poly_block}
let _p1 = pair("left"[String], 1[Int]);
let _p2 = pair(("left"[String], 1[Int]));
()
```

Both calls infer `A` as `String` and `B` as `Int`.

### Calls have stages

Each value group applies one function stage. A comma-separated group is one
product argument, so it agrees with an explicitly written tuple:

```kio {@tutorial_module}
fn add_then(left: Int, right: Int)(extra: Int) -> Int { add(add(left, right), extra) }

fn staged_example() -> Int { add_then(1, 2)(3) }

fn tuple_example() -> Int { add_then((1, 2))(3) }
```

The first stage consumes `Int & Int` and returns `Int -> Int`;
the second stage supplies the extra number. `add_then(1, 2, 3)`
would supply one three-component product to the first stage, so it does not
fit. To retain the returned function, stop after the first complete stage.
There is no partial application of half a product domain.

Type stages can appear between value stages too:

```kio {@tutorial_module}
fn keep_first[A](left: A)[B](_right: B) -> A { left }

fn explicit_stages() -> String { keep_first[String]("left")[Int](1) }

fn inferred_stages() -> String { keep_first("left"[String])(1[Int]) }
```

The brackets select types; each pair of parentheses supplies one value stage.
Omission still lets these connected stages cooperate through their ordinary
local type constraints. A `let` remains a boundary: once an initializer must
finish, a later statement cannot complete its missing types.

`f[A]` and `f [A]` mean the same type application;
to pass a square-bracket collection as a value, put it inside the value list,
for example `f([* a, b *])`.

`type` aliases and nominal entries can carry the same `[A]`, `[B]` shape — that's how generic containers and dictionaries get expressed without typeclasses or traits.

You rarely need to think about the full inference rules in a small program; when a call or local binding feels ambiguous, add the type where it helps the reader too.

### Type-only values

Some polymorphic values need a type but no value argument. Leave out the
value-parameter group to construct the result as soon as the type is supplied:

```kio {@tutorial_module}
fn nothing[A] -> A | . { widen_sum(())[A | .] }

fn nothing_later[A]() -> A | . { nothing[A] }

fn example() {
  let make = nothing;
  let _empty = make[Int];
  let wait = nothing_later[Int];
  let _also_empty = wait();
  ()
}
```

`nothing` has type `[A] A | .`: `nothing[Int]` constructs an empty result.
Saving `nothing` in `make` keeps it polymorphic and does not run its body.
`nothing_later` has type `[A] . -> (A | .)`: supplying `Int` leaves a function
that waits for `()`. Choose the type-only form when selecting the type should
construct the result; keep `()` when the caller should explicitly start a
later computation. The two types are distinct.

The anonymous form is `.[A] { body }`:

```kio {@block_expr}
let make = .[A] { widen_sum(())[A | .] };
let _empty = make[Int];
()
```

The lambda's result type comes from its body. For a named `fn`, omitting
`-> Result` still means a Unit result, as it does for other named functions.
Every signature needs at least one group: write `fn action()` for a Unit
argument, not a group-free `fn action`. Repeated type groups such as `[A][B]`
are separate steps; the body runs after the last written group is supplied.

## Chapter 8 — UFCS calls

[UFCS](../guides/ufcs.md) — Uniform Function Call Syntax — is another spelling for an ordinary call. It lets you put one value before the callee when the expression reads better from left to right.

```kio {@tutorial_module}
fn plus_one_prefix(n: Int) -> Int { add(n, 1) }

fn plus_one_ufcs(n: Int) -> Int { n.>add(1) }

fn show_ufcs(n: Int) { n.>int_to_string.>print }
```

`n.>add(1)` means `add(n, 1)`: insert the left-hand value into the first value-argument slot. `n.>int_to_string.>print` applies the same rewrite twice, so the expression reads in data-flow order without changing which functions are called.

For nested ordinary calls, UFCS can also keep the value being formatted at the
front of the line. The [UFCS guide](../guides/ufcs.md#choosing-a-spelling)
shows how `.>>` preserves a right-nested call's argument grouping.

UFCS does not make `add` a member of `Int`, and it does not search for operations by receiver type. The callee must already be in scope, and name lookup, type arguments, and inference are the same as for the prefix call.

An argless UFCS segment is bare: write `value.>f` rather than `value.>f()`.
The latter is rejected because direct `f()` means a Unit call; write
`value.>f(())` when Unit really is an additional argument.

Macro calls use the same surface rule. Because Chapter 6 already used `widen_sum`, you can read `(s.>widen_sum)[String | Int]` as the receiver-first spelling of `widen_sum(s)[String | Int]`. The full guide covers the other UFCS splice forms: `.>>`, `.<`, and `.<<`.

Blocks stay attached to the macro: `v.>match { clauses }` means
`match(v) { clauses }`. In a reverse spelling, the receiver follows all the
blocks: `match { clauses }.<v`. In either case it fills the scrutinee position,
not a position after the clauses. See the
[UFCS guide](../guides/ufcs.md#calls-with-trailing-blocks) for nested blocks
and grouping.

<!-- markdownlint-disable-next-line MD033 -->
<a id="chapter-9-imported-macros"></a>

<!-- markdownlint-disable-next-line MD033 -->
<a id="chapter-9--imported-macros"></a>

## Chapter 9 — Imported macros

Macros are Kio library code that the compiler runs while checking a call. A module declares a macro under an ordinary name. Callers select it explicitly, for example `import match {macro match}`, then call that local name directly. If `widen_sum` is not in scope, `widen_sum(...)` does not resolve.

The tutorial harness imports `match` from `match` and imports `widen_sum`, `one_prod`, and friends from `spine_macros`. A real package gets those modules through a dependency and writes the imports itself. Kio provides the macro mechanism; it does not globally install this palette.

Use a macro when the type already says what structural glue is needed and you do not want to write the glue by hand. The examples below use prefix calls so the imported names stay obvious; the UFCS forms from Chapter 8 are equivalent when you want receiver-first reading.

```kio {@tutorial_module}
fn as_text(v: String | Int) -> String {
  match(v) {
    .(s: String) { s };
    .(n: Int) { int_to_string(n) }
  }
}

fn as_sum(s: String) -> String | Int { widen_sum(s)[String | Int] }

fn pick_second(p: String & Int) -> Int { one_prod(p)[Int] }
```

Read those as three explicit requests to imported code:

- `match` builds branch selection for a sum.
- `widen_sum` injects a value into a target sum that contains its type.
- `one_prod` picks a slot out of a product by target type.

That is enough for this tutorial. The full spine palette includes `reorder_*`, `narrow_*`, `widen_*`, `flatten_*`, `one_*`, and `fit`; the checked implementations and per-form examples live in [the macro case study](../poc/macros.md). [Using libraries](../guides/using-libraries.md) explains how to adopt them. The [structural sums guide](../guides/sums.md) covers `match` in depth, and [Error handling](../guides/error-handling.md) shows how sums compose into explicit failure flow.

## Chapter 10 — Where to go from here

This tutorial covered the bones: packages, modules, functions, atomic types, tuples and nominal fields, sums, pattern matching, polymorphism, UFCS, and macro calls. Enough to read Kio code and write small programs.

The next layer is in [docs/guides/](../guides/), in roughly dependency order:

- [Structural sums and pattern matching](../guides/sums.md) — the full vocabulary for sum types, including the more interesting `match` patterns.
- [UFCS calls](../guides/ufcs.md) — the four receiver-splice spellings and how they map back to ordinary calls.
- [Using libraries](../guides/using-libraries.md) — adopting imported libraries; [the macro case study](../poc/macros.md) supplies checked per-form examples of the structural helpers.
- [Operators](../guides/operators.md) — fixed operator patterns and recursive / greedy slots.
- [Variadic operators](../guides/variadic-operators.md) — four fold modes, element order, and collection literals.
- [Row-typed records with ordinary products](../guides/products.md) — how `&` plus type variables expresses row polymorphism and field update.
- [Error handling](../guides/error-handling.md) — `(T | !)` sums and explicit failure flow.
- [Open-world story](../guides/open-world.md) — what the open-world property looks like in practice.

And [docs/poc/](../poc/) contains case studies that read through executable proof-of-concept packages end to end:

- [The optics library](../poc/optics.md) — lenses, prisms, and isos as function pairs, the operator DSL, and the spine palette in use.
- [Higher-kinded types](../poc/hkt.md) — kinded brands, instance dictionaries, `do`-block pipelines, and `derive`.

For the language as a contract — productions, precise semantics, type-system rules — read [`specs/language.md`](../../specs/language.md). For how to embed a Kio package in your environment, see [docs/hosts/](../hosts/). For the full `kio` command-line surface, [`specs/cli.md`](../../specs/cli.md).

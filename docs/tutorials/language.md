# The Kio language

Start here when you want to read and write ordinary Kio source. The chapters build a working model of the language one construct at a time: packages, modules, functions, host-backed values, data shapes, matching, polymorphism, UFCS, and elaborator calls.

Read straight through if you are new to Kio, or skip to the chapter that matches what you need. The goal is practical familiarity: enough to read modules, write simple programs, and recognize common surface forms. Deeper treatment of individual features, such as operator definitions, UFCS, elaborators, and optics, lives in [docs/guides/](../guides/).

<!-- Snippets here illustrate Kio at every level: package files
(variant=package), module bodies
(via a harness), and expression-shaped fragments (a block-position
harness wraps each in `pub fn body() -> . { ... }`). Each
chapter's harness picks up where the previous one left off. -->

<!-- Block-position harness for expression-shaped fragments: each
snippet body becomes the contents of a fresh fn. Same host
capabilities as the module-body harness below. -->
<!--kio {harness=block_expr placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import match(match);
import control(if, scope);
import spine_elaborators(narrow_prod, one_prod, one_sum, widen_sum);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String) -> .;

pub fn body() -> . {
  __INSERT_CODE_HERE__
}
-->

<!-- Top-level harness for module-body fragments — fn / type / literal /
labels / `pub fn main` declarations. -->
<!--kio {harness=tutorial_module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import match(match);
import control(if, scope);
import spine_elaborators(narrow_prod, one_prod, one_sum, widen_sum);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String) -> .;

__INSERT_CODE_HERE__
-->

## Chapter 1 — Your first Kio program

A Kio program is a **package** — a directory of files. The smallest interesting package has two kinds of files: a *package file* declaring the package's contract with its host (and, in an optional leading `build { ... }` block, naming the package's compilation targets), and one or more *modules* holding the actual code.

The canonical "hello, world" is one of each.

The package file lives at the package root. Its filename stem (`hello`) is the package's name. The optional `build { ... }` block comes first, naming a compilation target; then a `bridge { ... }` block lists the modules whose public surface forms the package's contract with its host.

```kio {variant=package}
// hello.pkg.kio
package hello;

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

```kio {variant=module}
// hello.kio
module hello;

host type String role(str);

host fn print(p0: String) -> .;
```

```kio {@tutorial_module}
// hello/main.kio
// In real code this would read:
//   module hello/main;
//   import hello(String, print);
// The tutorial's validation harness uses its own package name
// (`kiodoc`) and an expanded host surface — including `Int`, `add`,
// `int_to_string` — declared directly in that module, so the rest of
// the tutorial's snippets can reuse the same scope.
pub fn main() -> . { print("hello\n") }
```

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

read it as *the body of `pub fn main() -> . { ... }` in a module whose host supplies `String`, `print`, and the other capabilities the chapter assumes* — the same shape as the hello-world above, with those `host` items exposed by the package's `bridge` block when they cross the package boundary.

## Chapter 2 — Inside a module

The hello-world's `hello/main.kio` is a **module**. A module is a `.kio` source file with a `module` declaration at the top, optional `import` statements pulling names into scope, and a sequence of definitions — types, functions, operators. Modules group definitions; the *package* groups modules.

```kio {@tutorial_module}
// As in Chapter 1, real code would write:
//   module hello/main;
//   import hello(String, print);
pub fn main() -> . { print("hello\n") }
```

The `module hello/main;` line says "this file is the module at path `hello/main`." The path mirrors the directory layout: `hello/main.kio` is the module `hello/main`; `hello/util/string.kio` would be `hello/util/string`.

### Import statements

`import` brings names into the module's scope. The common shape pulls items from another module in the same package:

`import some/module(X);` lists one or more items, comma-separated. Bare items select ordinary values or types. A braced item selects label syntax, so a consumer that names a generated type and constructs its label writes `import people(Name, {name});`. Neither spelling imports the other. Host capabilities are no exception: `host type` and `host fn` declarations are ordinary public items of the module that declares them, so the hello-world brings in `String` and `print` with `import hello(String, print);`.

When another module is mostly used as a source of names, prefer the qualified alias form where that import surface supports it: `import some/module as m;` or `import utils/list_ops as l;`. The call site then keeps the origin visible, as in `m.helper(...)`. The alias also reaches exported types, type constructors, and newtype members: `m.Shape` names a type, `m.Stack(Item)` applies a type constructor, and `m.Stack.unpack` reaches a public newtype member. These remain qualified names; the alias does not add them to unqualified scope. Use selective imports when the bare name is the point of the example, part of the module's public vocabulary, or required by syntax. The special compile-time and core imports are the exceptions: `import __comptime__;` and `import __intrinsics__;` are block imports with no alias form. Ordinary programs rarely need `__intrinsics__`; use tuples, labels, and imported library elaborators such as `match!` and the structural palette for normal product and sum code.

An operator is selected by its complete tagged grammar, for example
`import arithmetic(op _ + __);` or `import list(varop [* *]);`.
The import supplies the syntax needed to parse this module; semantic checking
then requires an exact exported match in the named provider. Importing the
underlying function alone does not import its operator. See the
[operator guide](../guides/operators.md) for fixed patterns and the
[variadic-operator guide](../guides/variadic-operators.md) for collection syntax.

Selective imports have a nonempty parenthesized list even for one name. For a
long list, keep `(` on the module-path line and put each selected item on its
own line with a leading comma. Each ordinary name or module alias is introduced
once: repeating the same import is an error, even from the same provider. The
two fixed builtin block imports are idempotent.

### pub and fn

`fn name(args) -> Return { body }` defines a function. `pub` before `fn` makes the function visible to other modules — and, when the module is bridged, to the package surface, which is how the hello-world's `main` reaches the host. Without `pub`, the function is module-private.

Same idea for `type`, `literal`, `newtype`, and `labels`: `pub` makes them visible; declarations without `pub` are module-private. The package's `bridge` block exposes only the `pub` items of the modules it matches; non-`pub` items stay internal regardless.

### Definitions are read top-to-bottom

A module's declarations are processed in order, and each one sees only the names declared *above* it (plus whatever `import` brought in). There is no forward reference within a file: a `fn` that calls a helper must come *after* that helper's definition, and a `fn` cannot call itself by name. Order helpers before their callers. Host declarations follow the same rule: declare a `host type` before any signature that mentions it, and a `host fn` before code or a declaration that names it.

Recursive scope is explicit. `rec newtype` gives one nominal declaration its
own head; `rec labels` gives one label declaration its generated nominal heads
and, for a named form, its transparent alias head. A bare `rec { ... }` type
group gives exactly one genuinely mutual component access to all of its member
heads. None of these data forms sees an unrelated later declaration, and a
redundant marker or over-broad group is an error. A separate `rec(loop)` group
lets functions call each other through `rec name(...)`. Term recursion always
goes through that loop-capability form — see [Recursion](../guides/recursion.md)
and [Aliases, newtypes, visibility, and purity](../guides/declarations.md#recursive-newtypes).

When updating older source that relied on implicit recursion, add a singleton
marker only for a declaration that refers to its own data head:

```diff
-newtype List[A] : . | (A & List(A)) { constructor mk_list; projector un_list }
+rec newtype List[A] : . | (A & List(A)) { constructor mk_list; projector un_list }
```

The diagnostic is “recursive data declaration requires `rec`”; the editor
offers “Add `rec` to this recursive newtype” (or “Add `rec` to this recursive
labels declaration”). If the marked declaration is acyclic, “this `rec` marker
is unnecessary” offers “Remove unnecessary `rec`”. A negative, ill-kinded, or
wrong-arity recursive payload reports that type defect and offers no marker
edit.

For declarations that used to depend on a later peer, wrap exactly the mutual
cycle rather than a whole run of neighboring declarations:

```diff
-type Tree[A] = Branch(A);
-newtype Branch[A] : . | (A & Tree(A) & Tree(A)) {
-  constructor mk_branch;
-  projector un_branch
-}
+rec {
+  type Tree[A] = Branch(A);
+  newtype Branch[A] : . | (A & Tree(A) & Tree(A)) {
+    constructor mk_branch;
+    projector un_branch
+  }
+}
```

The ungrouped cycle reports “mutually recursive data declarations require
`rec { ... }`” and offers “Fix recursive type groups”. The same action repairs
an acyclic group (“this `rec` group contains no recursive cycle”) or splits an
over-broad group (“this `rec` group contains multiple independent
components”). An alias-only cycle instead reports “recursive type component
has no `newtype` boundary” and offers no grouping or marker shortcut. When one
file has independent group and singleton mistakes, apply the currently
reported repair first; reanalysis then offers the next one. Fix All combines
only repairs already visible in the current analysis.

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
module hello;

host type String role(str);

host fn print(p0: String) -> .;
```

`host type String role(str);` says "the host supplies a type, this module will call it `String`, and string literals checked against it produce values of that type." `role(str)` is the binding.

Role-bearing host types are atomic and therefore have no type parameters. A
roleless host type may still be parameterized, such as `host type Array[A];`;
`host type Array[A] role(i32);` is rejected.

Other roles cover numbers. A module that needs 32-bit integers and booleans declares them similarly:

```kio {variant=module}
// arith.kio
module arith;

host type Int role(i32);

host type String role(str);

host type Bool role(bool);

host fn add(p0: Int, p1: Int) -> Int;

host fn int_to_string(p0: Int) -> String;

host fn print(p0: String) -> .;
```

The package file admits that module's surface by listing it in `bridge`:

```kio {variant=package}
// arith.pkg.kio
package arith;

bridge {
  arith;
  arith/**
}
```

Inside `arith` and its submodules, the literal `42` produces an `Int`, `"hi"` produces a `String`, and `.t` / `.f` produce `Bool` values.

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
module temps;

host type F64 role(f64);

host fn add_f64(p0: F64, p1: F64) -> F64;

pub fn adjusted() -> F64 { add_f64(19.5(F64), 0.25(F64)) }
```

A literal's type is resolved in three tiers. First, an explicit `(Type)` annotation right on the literal — `42(Int)` — names the type directly. Second, if there's no annotation, the **expected type** from the surrounding position is used: a function-return type, or a call argument checked against a parameter type. Third, failing both, the literal resolves to the module's *unique* role-bearing host type whose shape it admits — here `arith` declares exactly one integer type, so an unannotated `42` in any position resolves to `Int`.

Multiple host types in one scope may carry the same role. If a module has two integer-shaped host types in scope, both declarations are valid, but an unannotated integer literal in a position with no expected type is ambiguous — you'd annotate it (`42(Int)`) to pick one. A module with no integer type at all can't type an integer literal: there's nowhere for the value to land.

When no direct role-bearing host type is in unqualified lexical scope, type aliases in that scope provide the fallback candidate pool. A qualified module import alone does not add its members; an in-scope alias can still name its terminal host type through that qualifier. Aliases do not create extra candidates when they name the same host type: if `First` and `Second` both name `provider.Bool`, conditionals still see one Boolean type. If they name `first.Bool` and `second.Bool` from two different modules, those are two types even though both declarations use the leaf name `Bool`; a conditional then needs the scope reduced to one Boolean-role type.

Operations on these types — `add`, `mul`, `int_to_string`, `print` — are `host fn`s. Kio does not auto-include a standard library: the host supplies whatever the bridged modules' `host` declarations ask for, and reusable Kio code arrives through ordinary modules and dependencies. The compiler provides the core and compile-time pseudo-modules named in Chapter 2, but libraries such as the structural elaborator palette are user Kio code that a package imports explicitly.

For the rest of the tutorial, assume the module declares `Int`, `String`, `Bool`, the obvious numeric ops, and `print` / `int_to_string` as `host` items. Each chapter's snippets sit inside `arith/main.kio` or its equivalent.

## Chapter 4 — Functions

A function is a `fn` at module level (or a `fn` expression inline).

```kio {@tutorial_module}
fn double(n: Int) -> Int { add(n, n) }
```

Parameters as `name: Type` pairs; return type after `->`; the body is a block. A block is one or more statements separated by `;`, ending in a trailing expression whose value is the block's value.

### let bindings

`let name = expr;` binds a name to a value for the rest of the block.

```kio {@tutorial_module}
fn quadruple(n: Int) -> Int { let twice = add(n, n); add(twice, twice) }
```

Write `let .(name: Type) = expr;` when the RHS needs that local expected type:

```kio {@tutorial_module}
fn add_one(n: Int) -> Int { let .(inc: Int -> Int) = .(x) { add(x, 1) }; inc(n) }
```

`let` may shadow an outer binding of the same name; the inner binding wins for the rest of the block.

### Expression statements

An expression followed by `;` runs the expression for its side effect and discards the value. The expression must have type `.` (unit) — most host I/O functions already return it.

```kio {@tutorial_module}
fn greet_twice() -> . { print("hi\n"); print("hi again\n") }
```

One statement, then a trailing expression: the second `print` is the block's tail. If you find yourself reaching for `let _ = e;` to throw away a value, the expression statement is the form you want — provided `e` is already unit-typed.

### Inline functions

A function value can appear inline as `.(args) { body }` — a lambda.

```kio {@tutorial_module}
fn apply(f: Int -> Int, x: Int) -> Int { f(x) }

fn immediate_identity() -> Int { .(n) { n }(41) }

pub fn main() -> . { let y = apply(.(n) { add(n, 1) }, 41); print(int_to_string(y)) }
```

A lambda's parameter types can be elided when the expected type is fixed by context — here, by `apply`'s signature. They can also be written explicitly (`.(n: Int) { add(n, 1) }`) for emphasis.

Checked annotations may mix concrete structure with local `_` placeholders,
but a placeholder cannot sit beneath a `forall` written inside that same
annotation. `.[A](x: _) -> _ { x }` remains valid because `[A]` is a header
binder and both annotations are whole slots. By contrast,
`.(f: [A] A -> _) { ... }` is rejected: the placeholder is inside `f`'s own
polymorphic annotation. Write the concrete result beneath that binder, or make
the whole `f` annotation `_` so the surrounding function slot supplies it.

An immediate call can also supply a whole missing parameter type:
`immediate_identity` gives `n` the independently known type of `41`. The rule
is local to that call. Binding `.(n) { n }` with an untyped `let` and calling
the binding later does not send type information backward across the `let`;
give the binding a concrete function-type annotation when it must be named
first.

### Conditionals

Importing `if` provides an `if!` call with two blocks. Both branches produce one common result type:

```kio {@tutorial_module}
fn choose(condition: Bool, when_true: Int, when_false: Int) -> Int {
  if! condition {
    when_true
  } else {
    when_false
  }
}
```

The condition must have the uniquely selected Boolean-role host type in the
current scope. Direct host types are selected when present; aliases provide the
fallback pool otherwise. A qualified module import alone does not add its
members. Multiple paths to one host declaration still count once, while
distinct declarations in the selected pool are ambiguous. Both arms are
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

The imported `scope!` elaborator turns an ordinary scoped block into an expression. It is useful in
an argument, conditional arm, or `equiv` arm where local statements need a
wrapper:

```kio {@tutorial_module}
fn increment_in_expression(value: Int) -> Int {
  scope! {
    let next = add(value, 1);
    next
  }
}
```

The body admits ordinary `let`, expression-statement, and trailing-expression
rules. `do! bind { let x <- ...; result }` is a distinct, bind-based form covered
in [Higher-kinded types](../guides/higher-kinded-types.md#monadic-do).

### Placeholder lambdas

A placeholder lambda is compact partial-application syntax. Its identifier
stem appears between adjacent dots and names its parameter references:

```kio {@tutorial_module}
fn apply_int(f: Int -> Int, value: Int) -> Int { f(value) }

fn increment_placeholder(value: Int) -> Int { apply_int(.x. { add(x1, 1) }, value) }
```

`.x. { add(x1, 1) }` lowers to a one-parameter lambda. Indexed references may
repeat (`.x. { pair(x1, x1) }`) or reorder (`.x. { pair(x2, x1) }`); each
reference uses a positive index without a leading zero, such as `x1` or `x2`.
The highest index determines the lambda's arity. Other ordinary value-name
stems ending in a letter, such as `.arg.`, work too; each nested placeholder
lambda owns a fresh stem. Parameter types come from the surrounding expected
function type, just as they do for an unannotated `.(x) { ... }` lambda.

## Chapter 5 — Tuples and labels

Two ways Kio composes data: **tuples** (anonymous products) and **labeled values**.

### Tuples

`(a, b)` is a 2-tuple value; its type is `(A & B)` — the `&` reads as "and." Product types model finite cartesian products, and tuple values are elements of those products. Kio does not have a separate nominal tuple family. It also does not pretend every parenthesization is the same syntax tree: Kio chooses right-associated binary products as the canonical representation. `()` is the zero-item product value, `(a)` is just grouping around `a`, and `(a, b, c)` means `(a, (b, c))`.

```kio {@tutorial_module}
fn make_pair(s: String) -> String & String { (s, "world\n") }

pub fn main() -> . {
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
fn show_pair_param((first: String, second: String)) -> . { print(first); print(second) }
```

Parameter patterns and destructuring lets bind the product once and project the named components directly. The pattern shape extends to nesting (`((a: A, b: B), c: C)`), wildcards (`(_: A, b: B)` — keeps the slot's type but doesn't name it), and an "as-pattern" form (`name: (a: A, b: B)`) that binds the whole product alongside its components.

### Labels

<!-- Labels-aware harnesses: tutorial_labels_module declares Greeting at
the top level; tutorial_labels_block makes the same labels scope
available to expression-shaped fragments. -->

<!--kio {harness=tutorial_labels_module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(narrow_prod, one_prod, one_sum, widen_sum);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String) -> .;

labels Greeting = { hello : String, goodbye : String };

__INSERT_CODE_HERE__
-->

<!--kio {harness=tutorial_labels_block placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(narrow_prod, one_prod, one_sum, widen_sum);

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String) -> .;

labels Greeting = { hello : String, goodbye : String };

pub fn body() -> . {
  __INSERT_CODE_HERE__
}
-->

A `labels` declaration introduces lowercase value labels and generated nominal types.

```kio {@tutorial_module}
labels Greeting = { hello: String, goodbye: String };
```

This declares two labels, `hello` and `goodbye`, each carrying a `String`, and generated types `Hello` and `Goodbye`. A label value is built with `{label = expr}` sugar:

A later named `labels` declaration can include the same generated nominal with
`hello: _`. The underscore is an explicit reference to the earlier local label,
not an inferred payload; see [Structural products and row types](../guides/products.md#labels-and-).

```kio {@tutorial_labels_block}
let hi = {hello = "hi\n"};
let bye = {goodbye = "bye\n"};
print(hi.?{hello});
print(bye.?{goodbye})
```

The type of `{hello = "hi\n"}` is `Hello`. The payload (`String`) is fixed at the `labels` declaration site and read off from there. Multiple labels compose into one value with multi-label construction:

```kio {@tutorial_labels_block}
let pair = {hello = "hi\n", goodbye = "bye\n"};
// pair : Hello & Goodbye
print(pair.?{hello});
print(pair.?{goodbye})
```

The generated type is the type spelling; the lowercase label is value syntax only. Reaching back into a labeled value uses field access or the generated `get` member:

```kio {@tutorial_labels_block}
let s = Hello.get({hello = "hi\n"});
// s : String, value "hi\n"
print(s)
```

Labels are the workhorse of "records with named fields" in Kio. The full vocabulary — construction, access, update, defining your own nominal wrappers via `newtype`, and named `labels Greeting = {…}` aliases — is covered in [Structural products and row types](../guides/products.md).

## Chapter 6 — Sums and pattern matching

Where `&` ("and") composes types into products, `|` ("or") composes types into **sums**.

```kio {@tutorial_module}
fn classify(v: String | Int) -> . {
  let _r =
    match! v {
      .(s: String) { print(s) };
      .(n: Int) { print(int_to_string(n)) }
    };
  ()
}
```

`(String | Int)` is the type of a value that is *either* a `String` *or* an `Int`. `match!` takes one **clause** per arm of the sum. Each `.(name: Type) { body }` matches one branch; the body computes the result.

This is a deliberate preview of elaborators. `match!` is imported library code, not a built-in keyword. The tutorial harness imports it for the snippets; in a real package, you depend on the module that defines `match` and bring it into scope with `import match(match);`. Chapter 9 comes back to the import model.

Every clause body must produce the same result type. Both arms above produce the unit value, so the `match!` expression has type `.`. We bind it to `_r` only to sequence the printing before returning `()`.

### Constructing a sum value

How do you build a `(String | Int)` value from a `String`? With another preview elaborator, `widen_sum!`, which injects a value into a target sum that contains its type as an arm:

```kio {@tutorial_module}
fn classify_again(v: String | Int) -> . {
  let _r =
    match! v {
      .(s: String) { print(s) };
      .(n: Int) { print(int_to_string(n)) }
    };
  ()
}

pub fn main() -> . { let v = widen_sum!("matched right\n", String | Int); classify_again(v) }
```

`widen_sum!("matched right\n", String | Int)` says "put this `String` into this target sum." Like `match!`, `widen_sum!` is imported library code; the tutorial harness imports it from `spine_elaborators`. The elaborator walks the target's spine for the arm a `String` fits (here, the left arm) and produces the sum value — the walk is scoped to the target type, not to what other types are in scope.

## Chapter 7 — Polymorphism

A `fn` whose type parameters appear in `[brackets]` is **polymorphic** — it works for any type substituted for the parameter.

Type-parameter names are local to their function. A function declared with `[Token]` can take a caller's unrelated type named `Token`: the type argument refers to the caller's type, not to the callee's parameter. Renaming that parameter to `[A]` throughout the function leaves its callers unchanged.

A nested lambda can declare the same type-parameter name as its enclosing
function. Inside the lambda, that name refers to the lambda's parameter;
outside it, the enclosing parameter is still in scope:

```kio {@tutorial_module}
fn identity[A](value: A) -> A { value }

fn shadow[A](value: A) -> A {
  let inner = .[A](other: A) -> A { identity(A, other) };
  inner(A, value)
}
```

Here `identity(A, other)` uses the inner `A`. The call `inner(A, value)`
supplies the outer `A`, so `shadow` returns the same type it received.

<!-- A poly-aware harness: declares `id` and a few call-site
contexts so the expression-shaped fragments below can call them. -->

<!--kio {harness=tutorial_poly_block placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type Int role(i32);
host type String role(str);
host type Bool role(bool);
host fn add(p0: Int, p1: Int) -> Int;
host fn int_to_string(p0: Int) -> String;
host fn print(p0: String) -> .;

fn id[A](x: A) -> A { x }

fn pair[A][B](x: A, y: B) -> (A & B) { (x, y) }

pub fn body() -> . {
  __INSERT_CODE_HERE__
}
-->

```kio {@tutorial_module}
fn id[A](x: A) -> A { x }
```

`id` takes a type argument `A` and a value `x` of that type, and returns `x`. Most call sites elide the type argument; the typer infers it from the value-arg's type:

```kio {@tutorial_poly_block}
let _i = id(42);
// A inferred as Int
let _s = id("hi");
// A inferred as String
()
```

Each type-argument slot in the current universal/function layer has three
spellings. Write every type explicitly, put `_` in only the slots the typer
should solve, or elide that layer's leading run:

```kio {@tutorial_poly_block}
let _explicit = pair(String, Int, "left", 1);
let _partial = pair(String, _, "left", 1);
let _elided = pair("left", 1);
()
```

Within that layer, written type arguments and `_` placeholders are positional
and precede its value arguments. In the partial call, `String` fills `[A]` and
`_` asks inference to solve `[B]` from `1`. Elision omits that leading binder
run; it is not shorthand for writing a variable number of underscores among
value arguments. Consuming a value layer may expose a later binder run, which
the same rule handles as the next layer of the call.

Bare polymorphic values stay polymorphic. When a surrounding function needs a
particular instance as a value, write `_` as the type-only application:

```kio {@tutorial_poly_block}
let _instantiated_function = id(_)(42);
()
```

Here `_` is solved as `Int`, so `id(_)` is the residual function
`Int -> Int`. `id()` means something different: its syntactically empty direct
call writes one Unit value, so it calls `id` at the Unit type and produces
`()`. Written `id(())` does the same thing because `()` is only a value.
`id(.)` explicitly selects the Unit type but writes no value, so it remains the
residual function `. -> .`; write `id(., ())` to apply it. Likewise,
`id(String)` retains `String -> String`. An expected function type may solve a
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

A nonempty list containing only type arguments remains a partial application.
For `id` above, `id(String)` has type `String -> String`, and `id(.)` has type
`. -> .`; write `id(., ())` to apply the latter function to Unit. A direct
empty call is different: `id()` omits `A`, writes one Unit value, infers
`A = .`, and produces `()`, just like `id(())`.

For a function `nil : [A] . -> List(A)`, `nil(A)` likewise retains
`. -> List(A)`, while `nil(A)()`, `nil(A, ())`, and `().>nil(A)` apply the Unit
argument. The shorter direct spelling `nil()` works only where the surrounding
expected type determines `A`, and lowers with both arguments explicit. A
receiver-only UFCS call omits its list: `x.>f` means `f(x)`. The spelling
`x.>f()` is rejected; use `x.>f(())` when the call passes Unit in addition to
the receiver.

Most of the time full elision does the right thing. When a local RHS needs a
checked position, put the annotation on the local binder:
`let .(f: Int -> Int) = .(x) { x };`.

Multi-parameter polymorphic functions work the same way:

```kio {@tutorial_module}
fn pair[A][B](x: A, y: B) -> A & B { (x, y) }
```

`pair` infers both `A` and `B` from the value-arg types. The tuple spelling is equivalent to the comma-separated spelling because call arguments and tuple values use the same right-associated binary-product shape:

```kio {@tutorial_poly_block}
let _p1 = pair("left", 1);
let _p2 = pair(("left", 1));
()
```

Both calls infer `A` as `String` and `B` as `Int`.

One argument list may continue through several function layers. Its grouping
is lexical rather than guessed from argument types: values followed by more
values fill product slots one at a time, while the final value in a packet may
be a whole product. For a function of type `(A & B) -> C -> R`, write
`f(a, b, c)` for three flat slots and `f(pair)(c)` when `pair : A & B` should
fill the first layer as one value. `f(pair, c)` does not try both readings.

This is not automatic currying. A function of two value parameters is not a function that takes one value and returns a function for the second. If you want partial application, write that shape explicitly, for example `A -> B -> R`, or return a lambda from another lambda.

`newtype`, `labels`, and `type` declarations can carry the same `[A]`, `[B]` shape — that's how generic containers and dictionaries get expressed without typeclasses or traits.

You rarely need to think about the full inference rules in a small program; when a call or local binding feels ambiguous, add the type where it helps the reader too.

## Chapter 8 — UFCS calls

[UFCS](../guides/ufcs.md) — Uniform Function Call Syntax — is another spelling for an ordinary call. It lets you put one value before the callee when the expression reads better from left to right.

```kio {@tutorial_module}
fn plus_one_prefix(n: Int) -> Int { add(n, 1) }

fn plus_one_ufcs(n: Int) -> Int { n.>add(1) }

fn show_ufcs(n: Int) -> . { n.>int_to_string.>print }
```

`n.>add(1)` means `add(n, 1)`: insert the left-hand value into the first value-argument slot. `n.>int_to_string.>print` applies the same rewrite twice, so the expression reads in data-flow order without changing which functions are called.

UFCS does not make `add` a member of `Int`, and it does not search for operations by receiver type. The callee must already be in scope, and name lookup, type arguments, and inference are the same as for the prefix call.

An argless UFCS segment is bare: write `value.>f` rather than `value.>f()`.
The latter is rejected because direct `f()` means a Unit call; write
`value.>f(())` when Unit really is an additional argument.

Bang calls use the same surface rule. Because Chapter 6 already used `widen_sum!`, you can read `s.>widen_sum!(String | Int)` as the receiver-first spelling of `widen_sum!(s, String | Int)`. The full guide covers the other UFCS splice forms: `.>>`, `.<`, and `.<<`.

## Chapter 9 — Imported elaborators

Elaborators are Kio library code that the compiler runs while checking a call. A module declares an elaborator under an ordinary name, callers import that name with `import`, and the call site adds `!`. If `widen_sum` is not in scope, `widen_sum!(...)` does not resolve.

The tutorial harness imports `match` from `match` and imports `widen_sum`, `one_prod`, and friends from `spine_elaborators`. A real package gets those modules through a dependency and writes the imports itself. Kio provides the elaborator mechanism; it does not globally install this palette.

Use an elaborator when the type already says what structural glue is needed and you do not want to write the glue by hand. The examples below use prefix calls so the imported names stay obvious; the UFCS forms from Chapter 8 are equivalent when you want receiver-first reading.

```kio {@tutorial_module}
fn as_text(v: String | Int) -> String {
  match! v {
    .(s: String) { s };
    .(n: Int) { int_to_string(n) }
  }
}

fn as_sum(s: String) -> String | Int { widen_sum!(s, String | Int) }

fn pick_second(p: String & Int) -> Int { one_prod!(p, Int) }
```

Read those as three explicit requests to imported code:

- `match!` builds branch selection for a sum.
- `widen_sum!` injects a value into a target sum that contains its type.
- `one_prod!` picks a slot out of a product by target type.

That is enough for this tutorial. The full spine palette includes `reorder_*`, `narrow_*`, `widen_*`, `flatten_*`, `one_*`, and `fit!`; the checked implementations and per-form examples live in [Using libraries](../guides/using-libraries.md). The [structural sums guide](../guides/sums.md) covers `match!` in depth, and [Error handling](../guides/error-handling.md) shows how sums compose into explicit failure flow.

## Chapter 10 — Where to go from here

This tutorial covered the bones: packages, modules, functions, atomic types, tuples and labels, sums, pattern matching, polymorphism, UFCS, and elaborator calls. Enough to read Kio code and write small programs.

The next layer is in [docs/guides/](../guides/), in roughly dependency order:

- [Structural sums and pattern matching](../guides/sums.md) — the full vocabulary for sum types, including the more interesting `match!` patterns.
- [UFCS calls](../guides/ufcs.md) — the four receiver-splice spellings and how they map back to ordinary calls.
- [Using libraries](../guides/using-libraries.md) — checked imported elaborator examples, including the structural helpers used throughout the docs.
- [Operators](../guides/operators.md) — fixed operator patterns and recursive / greedy slots.
- [Variadic operators](../guides/variadic-operators.md) — four fold modes, element order, and collection literals.
- [Structural products and row types](../guides/products.md) — how `&` plus type variables expresses row polymorphism and field update.
- [Error handling](../guides/error-handling.md) — `(T | !)` sums and explicit failure flow.
- [Open-world story](../guides/open-world.md) — what the open-world property looks like in practice.

And [docs/poc/](../poc/) contains case studies that read through executable proof-of-concept packages end to end:

- [The optics library](../poc/optics.md) — lenses, prisms, and isos as function pairs, the operator DSL, and the spine palette in use.
- [Higher-kinded types](../poc/hkt.md) — kinded brands, instance dictionaries, `do`-block pipelines, and `derive!`.

For the language as a contract — productions, precise semantics, type-system rules — read [`specs/language.md`](../../specs/language.md). For how to embed a Kio package in your environment, see [docs/hosts/](../hosts/). For the full `kio` command-line surface, [`specs/cli.md`](../../specs/cli.md).

# Hosting Kio in Java

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to Java and
calling into it from a Java host. The authoritative contract is
[`specs/backends/java.md`](../../specs/backends/java.md); this guide is the
narrative companion.

> **Known host-source compatibility caveat.** Removing a nullary bridged
> `host type` removes its package-root generic argument and changes retained
> signatures that depended on the selected Java type to use a deprecated
> declaration-owned carrier. Existing host source must drop that generic
> selection and delete or rewrite dependent method and adapter overrides. Java
> has no optional or default type argument that can preserve those spellings
> without imposing a current-host selection. See the Java contract's
> [host-type removal section](../../specs/backends/java.md#host-type-removal-cannot-preserve-generic-arity).
> When retained signatures require incompatible epochs of one exact nominal
> declaration, Java omits the affected methods and types; existing host source
> must delete or rewrite their overrides and generated-type references. See
> the contract's
> [incompatible-retained-epochs section](../../specs/backends/java.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-java).

## The build target

The concrete generation baseline is `v21`. Write `baseline v21`
to keep that compatibility contract across compiler upgrades, or `baseline latest`
to follow the compiler's newest baseline. Omission and `baseline ()` also follow
the newest baseline until `kio fmt` pins them to its concrete name. See
[baseline selection](../../specs/package.md#per-target-keys) for the shared rules.

A package emits to Java by declaring a `java` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target java {
    out "out/java/";
    baseline v21
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build java` writes four source files under the package's namespace
directory — the namespace defaults to the package name (an optional
`namespace "com.acme.greeter"` key inside the target block overrides
it), and the branded handle name is the title-word-cased final
segment for an ordinary source-shaped namespace:

```text
out/java/greeter/
├── Greeter.java       — the package handle (the typed facade)
├── GreeterHost.java   — the host contract interface
├── Shapes.java        — boundary shape declarations
└── KioRuntime.java    — internal runtime (not part of the contract)
```

The emitted source targets Java 21+ and uses only the standard library.
Because every top-level name lives under the package namespace, two Kio
packages load side by side in one host program without conflict.

Namespace `com.acme.my_pkg` keeps that Java package path and uses handle
`MyPkg`, instantiated by `MyPkg.create(host)`. Affixed source names and fixed
default collisions use `KioPkg_...` brands; other explicit final segments use
`KioNs_...`. See [the exact namespace rules](../../specs/backends/java.md#output-layout).

## Loading the package

A Java host brings the package online in three steps.

### 1. Compile the generated source

```sh
javac out/java/greeter/*.java
```

Compiling loads nothing and calls no host functions.

### 2. Implement the host contract

`GreeterHost` declares one typed method per `host fn` and one exact adapter
pair per `host type` the bridged modules declare. A host-function module path
without source `_` uses
`<module, / → _>__<leaf>`; a path containing `_` uses the exact
`KioItem_<encoded-path>__<leaf>` form. Apply word casing per source component
before escaping remaining `_` as `_u` and `/` as `_s`; callable leaves also
word-case (`data_api.do_work` → `KioItem_dataApi__doWork`). For a module
`greeter` declaring `print`:

```kio {variant=module}
module greeter;

host type String role(str);

host fn print(p0: String);
```

```java
import greeter.GreeterHost;

final class Stdout implements GreeterHost<String> {
  @Override public String KioHostBinding_greeter__String_fromBody(String value) {
    return value;
  }

  @Override public String KioHostBinding_greeter__String_toBody(String value) {
    return value;
  }

  @Override public void greeter__print(String text) {
    System.out.print(text);
  }
}
```

The `String` argument is this host's selection for the exact
`greeter.String` declaration. The two declaration-keyed adapters translate
that selected type to and from the runtime's private role value. The compiler
checks the whole contract: a missing or mistyped function or adapter is a
compile error, not a runtime surprise. Ordinary Java interface override
checking applies equally to host functions and declaration adapters.

### 3. Instantiate and invoke

```java
import greeter.Greeter;

Greeter<String> pkg = Greeter.create(new Stdout());
pkg.greeter.KioModule_main.main();
```

The returned handle is a typed namespace tree. A root module uses word casing
and Java identifier escaping; nested modules use
`KioModule_<exact-component>`, and public newtype handles use
`KioType_<exact-component>`. Exact components word-case before escaping
remaining underscores (`Native_token` → `KioType_NativeToken`). Word casing
preserves initial case and outer underscores; function and member leaves
follow it too (`mk_pair` → `mkPair`). An export `main` from module `greeter/main` is
reached through `pkg.greeter.KioModule_main.main()`.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target java {
    out "out/java/";
    baseline v21
  }
}

bridge {
  greeter;
  greeter/**
}
```

`greeter.kio` declares the host capability:

```kio {file}
module greeter;

host type String role(str);
host fn print(p0: String);
```

`greeter/main.kio` imports it and exports `main`:

<!--kio {file}
package greeter;

build {
  cache "out/.kio-cache/";

  target java {
    out "out/java/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_java_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_java_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() { print("hello from kio\n") }
```

Java host (`Main.java`):

```java
import greeter.Greeter;
import greeter.GreeterHost;

final class Stdout implements GreeterHost<String> {
  @Override public String KioHostBinding_greeter__String_fromBody(String value) {
    return value;
  }

  @Override public String KioHostBinding_greeter__String_toBody(String value) {
    return value;
  }

  @Override public void greeter__print(String text) {
    System.out.print(text);
  }
}

public final class Main {
  public static void main(String[] args) {
    Greeter<String> pkg = Greeter.create(new Stdout());
    pkg.greeter.KioModule_main.main();
  }
}
```

```sh
javac -d classes out/java/greeter/*.java Main.java
java -cp classes Main
```

Running the host prints:

```text
hello from kio
```

When a sealed package signature removes a host function whose frozen type
closure is representable, its Java method stays as a deprecated throwing
default for source compatibility but is absent from
live dispatch. A removed host type instead leaves the package's current generic
parameter list: deprecated declaration-owned `Shapes` types and throwing
default adapters keep frozen method signatures nameable without making the
binding mandatory. Source that explicitly supplied the removed generic type
argument must drop it; an old method or adapter override using the selected
Java type must be deleted or rewritten against the declaration-owned carrier.
Every history-only public support declaration and member is deprecated too,
while any identity also reached by live code remains live and nondeprecated.
Package execution never calls retained adapters or methods.

If sealed history contains incompatible declarations at one exact nominal
identity, no generated Java type can preserve both frozen signatures. The
generator omits the affected retained methods and type closure. Existing host
source must delete or rewrite their overrides and direct generated-type
references; current hosts remain live-only. See the caveat banner above.

## FFI shapes at a glance

Keep shared tuple fields and nested Kio data unchanged while a retained
value or closure can still use them, even after a call returns. Construct a
changed tuple instead. A host-managed mutable `Box` may change through its
declared `get` and `set` host functions while the tuple keeps the same box.
See [shared data stays unchanged](../../specs/backends/README.md#10-shared-data-stays-unchanged).

Supply values matching their declared types, including values transported
through generic or opaque storage. Java's static types and required casts are
not a recursive runtime validation guarantee. See
[host values satisfy their declared types](../../specs/backends/README.md#11-host-values-satisfy-their-declared-types).

Each nullary host-type declaration is an independent generic parameter on the
generated host, package handle, factory, namespaces, shapes, callbacks, and
newtype carriers that use it. You select the concrete Java type and implement
that declaration's exact `KioHostBinding_..._fromBody` / `_toBody` pair. The
role fixes only the private adapter-side value (`String`, `Boolean`, boxed
numeric types, or `java.math.BigInteger`); it does not merge declarations.
Two declarations may select the same Java class and still remain separate
generic positions.

A Kio literal reaches one of these bindings only when the literal's lexical
role matches that exact declaration's role. An annotation or expected type
selects the declaration but cannot change the role, and a roleless declaration
has no matching literal. Java then receives the value through the same
declaration-keyed adapter pair; no separate literal representation is exposed.

Products use `Shapes.Product<A, B>` records with `left()` and `right()`.
A three-part product is nested on the right; left nesting remains left
nesting. Sums use the sealed `Shapes.Sum<A, B>` interface with
`Shapes.Left<A, B>` and `Shapes.Right<A, B>` records. Both variants keep
both type arguments, even when only one payload is present.

Function values use `Shapes.Fn1<A, R>`: a lambda receives one domain value
and returns one result. For a Unit domain or result, use `Shapes.Unit`.
Named methods preserve the source's parameter groups instead:
`first(x: A, y: B)` has two Java parameters, while
`second(pair: A & B)` has one product parameter. A named Unit parameter
is omitted and a named Unit return is `void`; a generic parameter selected
as Unit still takes a value. A named method wider than 254 reference
parameters uses one prepared argument container with typed accessors and a
builder.

Generated Kio call chains, including calls with work left after the child
returns, keep their pending work in heap storage rather than growing Java's
call stack. Passing a Kio callable unchanged through host storage preserves
this behavior. A host-written lambda that calls it is a separate Java call;
host callbacks and synchronous re-entry retain Java's ordinary stack limits.
See the shared [calling convention](../../specs/backends/README.md#calls-with-work-remaining)
for the resource and compatible-artifact boundaries.

Declaration-owned types follow their source module paths under `Shapes`:
`api/types.Pick_first` becomes `Shapes.KioModule_api.KioModule_types.Pick_first`.
Module holder classes have a fixed `KioModule_` prefix; type leaves retain
their Kio spelling. Keeping word boundaries makes names such as `Foo_bar`
and `Foobar` distinct even on case-insensitive filesystems. Import a leaf
directly when it improves readability.
A nullary newtype owns its carrier class; a parameterized declaration owns
its constructor-marker interface at that same readable path.

An unsaturated nominal constructor therefore uses a marker such as
`Shapes.KioModule_api.Box`.
Applying it uses `Shapes.Apply1<F, A>`; additional arguments nest another
`Apply1` around that index. Concrete and generic uses have the same type,
so passing a product, sum or callable through your opaque host container
does not require a payload conversion. A host declaration's own
`Shapes.KioModule_api.Box.Storage.fromNative` and `toNative` helpers store and
retrieve host-owned storage at that exact index. There is no general
`Apply1` payload operation.

Repeated enclosing names and type leaves entering the generated naming
frames have fixed role-bearing escapes. A host type named `Storage` uses `KioStorage` for its
storage helper. Exceptionally long or deep nominal paths use a bounded
root-level identity name; adding unrelated declarations never changes an
existing path. The exported root module `java` is reached through
`pkg.KioModule_java`, protecting Java's package qualifier; functions named
`java` remain ordinary `java()` methods. The precise escape and whole-classfile bounds are specified
in [Item naming](../../specs/backends/java.md#item-naming).

A public newtype always has its per-type handle under the module namespace.
Nullary declarations have an exact declaration-owned Java class;
parameterized declarations use their marker's nested `Apply1` index.
Visibility controls the operations available in either form:

- With neither member public, values are opaque.
- With only the constructor public, you can create values but not inspect
  their payloads.
- With only the projector public, you can inspect values received from Kio
  but cannot create them.
- With both public and no existential, the ordinary members expose the
  prepared payload. A nullary class also has a typed payload constructor and
  `value()` accessor, including for recursive payloads.

A first-class callable with its own type binder implements a generated
interface with one generic `advance()` method. Select the binder with
Java's ordinary method type-argument syntax, then use the returned value;
another binder requires another advance. A host implements such an interface
with an anonymous class, since Java lambdas cannot implement generic methods.
Advancing a type stage and calling a returned function are distinct
operations, so keep each stage's host effects at the operation that owns them.

For a polymorphic identity `[A] A -> A`, the interface is
`Shapes.KioForall_Type_Fn_T0_T0`. Its anonymous class overrides
`public <A> Shapes.Fn1<A, A> advance()` and returns `value -> value`.
Calling `identity.<String>advance().apply("hello")` selects the binder and
then invokes that returned function. The readable shape name is independent
of the declaration supplying the value; captured types become ordinary
interface type arguments. The [item-naming reference](../../specs/backends/java.md#item-naming)
explains nested binders, captured types and the bounded spelling for large shapes.

An existential projector returns the declared type stages and continuation
stage after accepting its receiver. Its continuation receives the hidden
payload without exposing the hidden type to the caller. That callable uses
the same one-domain rule, including an explicit `Shapes.Unit`.

See [`specs/backends/java.md`](../../specs/backends/java.md) for the full
boundary contract and [`specs/backends/README.md`](../../specs/backends/README.md)
for cross-cutting rules shared by every backend.

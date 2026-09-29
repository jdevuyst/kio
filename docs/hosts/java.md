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

A package emits to Java by declaring a `java` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target java {
    out "out/java/"
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

host fn print(p0: String) -> .;
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
    out "out/java/"
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
host fn print(p0: String) -> .;
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

pub fn main() -> . { print("hello from kio\n") }
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

Structural products and sums use payload-generic nominal shells in
`Shapes.java`. Products through 254 fields are records; wider products have
typed accessors and a typed builder so they do not exceed the JVM constructor
slot limit. Sums are sealed interfaces with typed record variants and
`KioMatch`; above 254 arms, the method takes one exact product companion whose
typed members are the case handlers. Function values use generic `FnN` /
`ProcN` functional interfaces, so a lambda supplies the flat prepared value
arguments through 254 reference slots. A wider function takes one exact
prepared product shell; exported and host instance methods use the same cutoff
and the whole-head shell's typed builder.
An unsaturated nominal type constructor has an exact category-specific
`KioNewtypeMk_<declaration>` or `KioHostTypeMk_<declaration>` marker.
`ApplyN` is a typed public carrier. The package handle's exact
`KioNewtypeApplication_<declaration>_lift` / `_project` and
`KioHostTypeApplication_<declaration>_lift` / `_project` methods are the typed
bridge between a saturated declaration carrier and that application; its
public API never asks host code to cast through `Object`.

A public newtype in a bridged module always has an exact declaration-keyed
carrier and a per-type handle under its module namespace, even if that handle
has no methods:

- With neither constructor nor projector public, values use an opaque Java
  class.
- With only the constructor public, the host can create values but cannot
  inspect them.
- With only the projector public, the host can inspect values received from
  the package but cannot create them.
- With both public and no existential, the carrier is a record with its typed
  prepared payload, including recursive nominal references.

The opaque and one-member forms keep their storage out of the public facade;
only the selected package methods, if any, provide public construction or
inspection. A callable-head type stage becomes a Java method type parameter.
A first-class callable that quantifies its own type uses an exact generated
functional interface whose generic `apply` method carries that type parameter
and the callable's value stages. Java lambdas cannot implement a generic
abstract method, so supply such a value with an anonymous class; its public
signature contains no `Object`. An
existential projector takes a continuation so its hidden type cannot escape.
Canonical Unit contributes no continuation argument; every other payload
contributes one.
Same-named newtypes in different modules remain different Java types, and an
opaque or one-member newtype stays a single value when nested in another
boundary shape. A unit return is `void`.

See [`specs/backends/java.md`](../../specs/backends/java.md) for the full
boundary contract and [`specs/backends/README.md`](../../specs/backends/README.md)
for cross-cutting rules shared by every backend.

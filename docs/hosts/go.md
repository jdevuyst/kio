# Hosting Kio in Go

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

> **Known host-source compatibility caveat.** Removing a nullary bridged
> `host type` removes its generic argument from every package-root-generic
> spelling, including the generated host interface, handle, factory, and
> `Env_` / `Exp_` aliases. Existing host source must drop that selection
> because Go has no optional or default type argument. See the Go contract's
> [host-type removal section](../../specs/backends/go.md#host-type-removal-cannot-preserve-generic-arity).
> When retained signatures require incompatible epochs of one exact nominal
> declaration, Go also omits the affected aliases and types; existing host
> source must delete or rewrite method signatures and other references that
> name them. See the contract's
> [incompatible-retained-epochs section](../../specs/backends/go.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-go).

This guide walks through compiling a Kio package to Go and
calling into it from a Go host. The authoritative contract is
[`specs/backends/go.md`](../../specs/backends/go.md); this guide is the
narrative companion.

Generated Go packages require **Go 1.26 or newer**. A module that builds
one declares `go 1.26`; this is ordinary stable Go and requires neither a
`toolchain` directive nor `GOEXPERIMENT`.

## The build target

A package emits to Go by declaring a `go` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target go {
    out "out/go/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build` or `kio build go` writes a self-contained Go package — a
single flat directory, since a Go package *is* a directory:

```text
out/go/
├── pkg.go          // generic handle, factory, and module functions
├── host.go         // generic host interface and exact role adapters
├── shapes.go       // generic products, row-anchored sums, nominal carriers
├── ffi.go          // stable slot aliases, sum cases, and constructors
└── kio_runtime.go  // internal: the canonical Unit
```

Every file declares `package greeter` — the package's **namespace**,
which defaults to the Kio package name and is overridden by the
`namespace` target key. You wrote the manifest, so you already know it.
If the default would produce the handle `Unit`, `Product`, `Sum`, or `KioSum`,
Kio uses an exact reserved default namespace to avoid the package's fixed
generated types (`product` → `__kio_pkg_product`, handle `KioPkg_Product`).
Go keywords and unimportable `main` / `internal` use the same rule.
An explicit namespace that produces one of those
four exact handles is rejected; near misses such as `united` and `product_row`
remain valid. Reserving the names up front keeps an existing handle stable if
the package later exposes another structural value.
The branded surface names all derive from it: the handle `Greeter` (the
namespace's public title brand), the host interface `GreeterHost`, the factory
`CreateGreeter`. Go import qualification scopes the fixed support names to
their emitted package, while those branded names stay namespace-derived, so
two Kio packages with distinct namespaces coexist in one Go build.

Namespace `my_pkg` retains the package clause `my_pkg`, but the handle is
`MyPkg` and its factory is `CreateMyPkg`. Affixed source names, reserved
defaults, and non-source explicit namespaces use the exact brands specified
in [the namespace rules](../../specs/backends/go.md#output-layout).

## How Go hosts the package

The generated facade is statically typed: the host interface, exported
methods, generic products, concrete sums, and public-newtype operations all
use ordinary Go types, so a mismatch is a Go compile error. Each nullary Kio
host-type declaration is an independent type parameter on the generated host
interface, package handle, namespaces, and factory. The host selects those
types explicitly; two declarations do not collapse merely because both use the
same Kio role or the same underlying Go representation.

A free function-local Kio type variable or existential position surfaces as
`any`. A parameterized host declaration instead uses its own generated
`KioHostType_...[...]` carrier, preserving both the declaration and its exact
type arguments while the erased package body continues to store an `any`.

## Loading the package

A Go host brings the package online in three steps.

### 1. Make the package importable

Point your build at the emitted directory (a `replace` directive in a
multi-module setup, a vendored copy, or a local module path — whatever
your project uses). The package qualifies as its namespace — `greeter`
here:

```text
// go.mod
module ffihost

go 1.26

require greeter v0.0.0
replace greeter => ./out/go
```

### 2. Implement the host interface

The generated `GreeterHost` interface in `host.go` declares one method
per `host fn` the bridged modules declare. For module paths without `_`,
method names carry the module namespace — `/` becomes `_`, then `__`
separates the leaf — and the **first letter is capitalized so the method
is exported**. A `host fn print` in module `greeter` therefore becomes
`Greeter__print`. For paths that contain `_`, Kio uses the collision-proof
form `KioItem_<encoded-module>__<leaf>`, word-casing each source component
before escaping remaining `_` as `_u` and `/` as `_s`. Both forms word-case
the callable leaf: `data_api.do_work` becomes `KioItem_dataApi__doWork`.
A host implementation lives
in your own package, and Go lets it implement only exported interface
methods.

```kio {variant=module}
// greeter.kio
module greeter;

host type String role(str);

host fn print(p0: String) -> .;
```

```go
package main

import "greeter"

type HostString string

type MyHost struct{}

func (MyHost) KioHostIn_greeter_String(value HostString) string {
    return string(value)
}

func (MyHost) KioHostOut_greeter_String(value string) HostString {
    return HostString(value)
}

func (MyHost) Greeter__print(arg0 HostString) {
    print(string(arg0))
}
```

A role-typed declaration adds the two declaration-keyed adapter methods shown
above. Word-cased ASCII-alphanumeric components use readable names joined by
`_` (`data_api.Native_count` → `dataApi_NativeCount`); remaining affixes or
otherwise ambiguous names use the exact frame. Source-readable frames
word-case components before escaping and calculating their byte lengths.
They convert the host-selected public type to and from the private Go type used
for the Kio role. Selecting `string` directly would make both adapters
identities; selecting `HostString` proves that the public declaration remains
distinct. A roleless nullary declaration needs no adapters. Go interface
satisfaction is structural — `MyHost` satisfies `GreeterHost[HostString]`
because it has the right methods, with no explicit declaration. If the emitted
interface ever drifts from what you implement, your host fails to compile; the
interface is the contract.

For `host type Box[A]`, generated host signatures use the
declaration-owned `KioHostType_...[A]` carrier. Host integration code may wrap
and unwrap the erased runtime payload through the carrier's explicitly unsafe
`UnsafeKioHostType_...FromNative` and `UnsafeNative` operations; ordinary host
methods retain the exact carrier application in their signatures.

When a later package signature removes a `host fn`, the generated interface
simply drops that method. Your host may leave its extra method in place because
Go interface satisfaction accepts a superset. If that method names generated
boundary types and its retained epochs are compatible, the old `Env_...`
aliases and the generic-shell or nominal declarations they need remain
available from the frozen signature; the removed
method itself is not generated and cannot be called by the package. Every
exported declaration kept only for that history has a standard
`// Deprecated: …` directive. A declaration also used by live source remains
ordinary and is not marked deprecated.

Current host type arguments and adapter methods cover live `host type`
declarations only. If a removed host type remains in an old signature, its
history-only Go nominal or parameterized carrier is deprecated and
source-addressable, but a new host neither selects it nor implements adapters
for it. Removing a nullary host type changes the generated generic arity, so an
existing host must drop that old type argument; Go cannot make that argument
optional.

If sealed history contains incompatible declarations at one exact nominal
identity, no generated Go type can preserve both frozen signatures. The
generator omits every affected retained alias and type. Existing host source
must delete or rewrite any extra method signature or direct reference that
names an omitted declaration; current hosts remain live-only. See the caveat
banner above.

### 3. Instantiate and invoke

```go
pkg := greeter.CreateGreeter[HostString](MyHost{})
pkg.Greeter.KioModule_main.Main()
```

`CreateGreeter[HostString]` returns a `*Greeter[HostString]` whose surface mirrors the bridged
exports under their module namespace. Root modules keep a natural exported
component: unmarked source names use title word casing; leading or trailing
underscore runs use `KioItem_...` with word casing before exact escaping.
Every nested module instead uses
`KioModule_<exact-component>`, and every public-newtype handle uses
`KioType_<exact-component>`. Thus `foo_bar` as a root module is `FooBar`,
the same source component nested under another module is
`KioModule_fooBar`, and newtype `Foo_bar` is `KioType_FooBar`. Function
and newtype-member method names keep the natural/exact component rule. An
export `main` from module `greeter/main` is therefore reached as
`pkg.Greeter.KioModule_main.Main()`. The host value is held for the package's
lifetime.

## Structural values at the boundary

When a host fn or an exported fn takes or returns a structural value,
use its stable `Env_...` or `Exp_...` alias from `ffi.go`. Compound values,
`any`, `Unit`, and functions have aliases at every public slot. A direct
nullary host binding uses the package-root type parameter itself because Go
does not permit a generic alias whose right-hand side is only that parameter.

- A **product** is a flat generic `struct` application. The ordinary
  positional pair is `Product[T0, T1]` with exported fields `F0` and `F1`;
  the stable site alias fixes the concrete arguments, so you construct it
  with an ordinary struct literal.
- A **sum** is a concrete `KioSum[Row]` hidden behind its stable site alias.
  Construct arm `N` with `New<alias>_N(value)`. Inspect it with `.Case()`, a
  type switch on `<alias>_N`, and the case's typed `.Value()`. The zero value
  is usable and selects the first arm with that payload type's zero value.

A callback written as `. -> R` is a nullary Go `func() R`. A generic
one-argument callback such as `A -> R` keeps that argument when `A` is later
set to Unit, so its Go type is `func(Unit) R`.

For example, for illustrative host-function return aliases:

```go
pair := greeter.Env_Api__pair_ret{
    F0: int32(7),
    F1: "seven",
}
use(pair.F0)
use(pair.F1)

choice := greeter.NewEnv_Api__choice_ret_0("left")
switch arm := choice.Case().(type) {
case greeter.Env_Api__choice_ret_0:
    use(arm.Value())
case greeter.Env_Api__choice_ret_1:
    use(arm.Value())
}

var zero greeter.Env_Api__choice_ret
switch arm := zero.Case().(type) {
case greeter.Env_Api__choice_ret_0:
    use(arm.Value())
}
```

Nested aliases follow the value you are navigating: product field `N` adds
`_fieldN`, a sum arm payload adds `_N_value`, and a callback adds `_cbargN` or
`_cbret`. A nested sum gets its own indexed case aliases and `New...`
constructors at that derived prefix. Polymorphic type binders add no alias
suffix.

Generic shell names depend on structural keys, not concrete payload types, and
do not use content hashes or registry-order suffixes. Stable site aliases are
the names normal host code should use.

For public-newtype members, the stable alias keeps the module, newtype, and
member boundaries distinct: `A_b.c` in module `api` uses
`Exp_KioItem_api__AB__c_<slot>`, while `A.b_c` uses the readable
`Exp_Api__A_bC_<slot>`.

## Newtypes

Every public newtype in a bridged module has its own handle under its module namespace, even
when that handle has no methods. The methods on it match the members the
package made public:

- With no public constructor or projector, values use an opaque Go type.
- With only a public constructor, the host can create values but cannot
  inspect them.
- With only a public projector, the host can inspect values received from
  the package but cannot create them.
- With both public, a non-recursive newtype keeps Go's transparent
  representation. Its ordinary constructor and projector use the payload type
  directly.

The opaque and one-member forms use a concrete, non-generic
`KioNewtype_V1_...` carrier whose name encodes the complete module-and-type
identity; their storage stays private. Their public methods retain exact host
bindings and declaration-owned host-type applications. A free method-local
polymorphic position remains `any`. An
existential projector accepts a continuation instead of returning its payload
directly. Canonical Unit contributes no continuation argument, a product
contributes one argument per flat right-spine slot, and every other payload
contributes one. This allows the host to use the hidden value without returning
its hidden type. A recursive both-member newtype uses Go's erased `any` boundary
representation unless an opaque or one-member carrier cuts the boundary
cycle first.

Same-named newtypes in different modules therefore remain different Go types.
When an enclosing newtype contains such a carrier, it stays a single boundary
value rather than exposing its hidden payload. Normal host code reaches it
through the newtype handle's methods and the stable `Exp_...` slot aliases.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target go {
    out "out/go/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`greeter.kio`:

```kio {file}
module greeter;

host type String role(str);
host fn print(p0: String) -> .;
```

`greeter/main.kio`:

<!--kio {file}
package greeter;

build {
  cache "out/.kio-cache/";

  target go {
    out "out/go/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_go_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_go_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() -> . { print("Hello from Kio!\n"(String)) }
```

Build it, then host it:

```go
package main

import "greeter"

type HostString string

type MyHost struct{}

func (MyHost) KioHostIn_greeter_String(value HostString) string {
    return string(value)
}

func (MyHost) KioHostOut_greeter_String(value string) HostString {
    return HostString(value)
}

func (MyHost) Greeter__print(arg0 HostString) {
    print(string(arg0))
}

func main() {
    pkg := greeter.CreateGreeter[HostString](MyHost{})
    pkg.Greeter.KioModule_main.Main()
}
```

Running this prints `Hello from Kio!` and exits 0.

## Where this leads

- [`specs/backends/go.md`](../../specs/backends/go.md) — the full backend
  contract.
- [`specs/backends/README.md`](../../specs/backends/README.md) — cross-cutting
  FFI properties shared by every backend.
- [Getting started with the Kio tooling](../tutorials/tooling.md) — build and
  check a package before loading it in a host.
- [`specs/cli.md`](../../specs/cli.md) — the exact `kio build` command
  contract.

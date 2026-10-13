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
Go keywords and unimportable `main` / `internal` receive an escaped default
namespace; explicit namespaces cannot use those names or the blank identifier
`_`. Names such as `unit`, `product`, and `my_pkg` stay unchanged.

The public roles are always `Package`, `Host`, and `New`. Hosts write
`greeter.New(host)`, name `greeter.Package` and implement `greeter.Host`.
An import alias works the same way: `g.New(host)`. Go's import qualification
keeps two packages' roles and support types distinct. See the precise
[namespace rules](../../specs/backends/go.md#output-layout).

## How Go hosts the package

The generated facade is statically typed: the host interface, exported
methods, generic products, concrete sums, and public-newtype operations all
use ordinary Go types, so a mismatch is a Go compile error. Each nullary Kio
host-type declaration is an independent type parameter on the generated host
interface, package handle, namespaces, and factory. The host selects those
types explicitly; two declarations do not collapse merely because both use the
same Kio role or the same underlying Go representation.

For generic calls, `Prepared[I]` carries a value at an exact Kio type.
The generated `Index_...` aliases name declarations, and `IndexExp_...` /
`IndexEnv_...` aliases name types at public argument and result positions.
Two declarations keep different indices even when you select the same Go
type for both. A parameterized host declaration uses its own generated
`KioHostType_...[...]` carrier with those indices as its type arguments.

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

Kio emits Go source files, so this separate-module arrangement also needs
`out/go/go.mod`. Create it after `kio build go`, which replaces the generated
directory:

```text
module greeter

go 1.26
```

The `module greeter` path matches the host's `require` and `import "greeter"`.
Recreate this manifest after each build; the worked example shows the commands.

### 2. Implement the host interface

The generated `Host` interface in `host.go` declares one method
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

host fn print(p0: String);
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
satisfaction is structural — `MyHost` satisfies `Host[HostString]`
because it has the right methods, with no explicit declaration. If the emitted
interface ever drifts from what you implement, your host fails to compile; the
interface is the contract.

For `host type Box[A]` in module `api`, generated host signatures use the
declaration-owned `KioHostType_api__Box[I]` carrier, where `I` is the exact
index for the selected Kio argument. Host integration code may wrap
and unwrap the erased runtime payload through the carrier's explicitly unsafe
`UnsafeKioHostType_api__BoxFromNative` and `UnsafeNative` operations; ordinary host
methods retain the exact carrier application in their signatures. For example,
`greeter.UnsafeKioHostType_api__BoxFromNative[greeter.Index_api__Number[int]](storage)`
wraps a native host container for that declaration, and
`box.UnsafeNative()` retrieves it. Store
Kio payloads inside that container as `Value`, using the typed helpers below.
These unsafe operations do
not check that erased native storage agrees with its selected Go type index.
Keep that storage consistent with the selected type arguments, including when
a generic host function returns its payload. See the
[host-type contract](../../specs/backends/go.md#atomic-types).

A generic host-interface method has no Go method-local type parameters.
Its unselected payload argument is `Value`; an application such as `Box[A]`
uses `KioHostType_api__Box[KioErasedIndex_S]` in that method's signature.
The selected application you pass to a generic export still uses its exact
index. Follow the generated method signature and keep erased storage
consistent with the Kio type promised by that operation.

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
pkg := greeter.New[HostString](MyHost{})
pkg.Greeter.KioModule_main.Main()
```

`New[HostString]` returns a `*Package[HostString]` whose surface mirrors the bridged
exports under their module namespace. Root modules keep a natural exported
component: unmarked source names use title word casing; leading or trailing
underscore runs use `KioItem_...` with word casing before exact escaping.
Every nested module instead uses
`KioModule_<exact-component>`, and every public-newtype handle uses
`KioType_<exact-component>`. Thus `foo_bar` as a root module is `FooBar`,
the same source component nested under another module is
`KioModule_fooBar`, and newtype `Foo_bar` is `KioType_FooBar`. Function
and non-generic newtype-member method names keep the natural/exact component
rule. An
export `main` from module `greeter/main` is therefore reached as
`pkg.Greeter.KioModule_main.Main()`. The host value is held for the package's
lifetime.

## Structural values at the boundary

Keep shared tuple fields and nested Kio data unchanged while a retained
value or closure can still use them, even after a call returns. Construct a
changed tuple instead. A host-managed mutable `Box` may change through its
declared `get` and `set` host functions while the tuple keeps the same box.
See [shared data stays unchanged](../../specs/backends/README.md#10-shared-data-stays-unchanged).

Keep values consistent with their declared types, including the payloads of
unsafe erased storage. Go's static types and required assertions do not promise
an early check of every stored value. See
[host values satisfy their declared types](../../specs/backends/README.md#11-host-values-satisfy-their-declared-types).

When a host fn or an exported fn takes or returns a structural value,
use its stable `Env_...` or `Exp_...` alias from `ffi.go`. Compound values,
`Value`, `Unit`, and functions have aliases at every public slot. A direct
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

A Kio declaration `fn first(x: A, y: B)` takes two Go arguments. A declaration
`fn second(pair: A & B)` instead takes one product struct, even if it
destructures that parameter. This also applies to host methods and newtype
members. Multiple parameter groups in a declaration head join one native
call; a function returned by that call remains a separate callable value.

A callback written as `(A & B) -> R` takes one product argument. A callback
written as `. -> R` is a nullary Go `func() R`. A generic
one-argument callback such as `A -> R` keeps that argument when `A` is later
set to Unit. In a selected generic view, pass its exact prepared Unit value.

A polymorphic value `[A] R` has a generated `TypeStage` alias. Select its
type with `Select<slot-name>[ChosenIndex](stage)`, then use the returned
function or value. Each selection runs that stage once; repeated calls to a
returned function reuse the selection. Selecting the original stage again
runs it again. Its `Invoke()` method runs one stage using its abstract result
view. Storing, preparing, transporting, or ignoring a stage runs nothing.
Nested type binders in the selected view require one selection each. After
`Invoke()`, later binders use ordinary nullary functions in the abstract
view; call each once to advance to its result.

The selection helper takes the stage alone. It keeps the host conversion
context already associated with that view. If you request a view through
another compatible package instance, that view uses the other instance's
adapters; the Kio callback still retains its original captures and host
bindings. To supply a Go-authored stage, use its generated
`New<slot-name>Stage(host, func() R)` helper. The function waits until
selection or invocation to run. When you pass it into another instance,
the receiving instance supplies its argument and result conversions.

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
constructors at that derived prefix. A polymorphic stage's abstract result adds
`_result`; its selected view adds `_selected`. A host carrier's static type
argument adds `_typeargN`.

Generic shell names depend on structural keys, not concrete payload types, and
do not use content hashes or registry-order suffixes. Stable site aliases are
the names normal host code should use.

For public-newtype members, the stable alias keeps the module, newtype, and
member boundaries distinct: `A_b.c` in module `api` uses
`Exp_KioItem_api__AB__c_<slot>`, while `A.b_c` uses the readable
`Exp_Api__A_bC_<slot>`.

### Passing values through generic slots and host containers

At a slot with an available exact index, use `Prepare<slot-name>` to pass a
native value through an exact generic slot, and `Unprepare<slot-name>` to
obtain its native view again. For example,
`greeter.PrepareExp_Api__pair_ret(pkg, pair)` returns a prepared value whose
index is `IndexExp_Api__pair_ret`. A nullary host declaration has its own
`greeter.PrepareHost_api_Number(pkg, number)` helper; this keeps `Number`
distinct from another declaration that also uses Go `int`. For parameterized host
declarations, use the free helper, such as
`greeter.PrepareHost_api_Box(pkg, box)`.
All slot and host-declaration `Prepare...` / `Unprepare...` helpers are free
functions with the package handle first. Go normally infers their type parameters from the
package and the typed value.

The input is an ordinary typed product; its fields need no manual casts or
conversion tags. The same operations cover sums, callbacks, and public
nominal carriers. Direct concrete calls still use their native types.

A generic export or public newtype member uses a free `CallExp_...`
function, because Go methods cannot add type parameters. Pass the module
namespace or newtype handle first, followed by the value arguments. For
example, `greeter.CallExp_Api__Box_make(pkg.Api.KioType_Box,
greeter.PrepareHost_api_Number(pkg, number))` selects the payload's exact type
from the prepared argument. When inference needs help, give the selected index
first in the generic argument list. Package host selections follow and
are normally inferred from the namespace.

For erased storage, `prepared.Erase()` returns a `Value`. The generated
`Value<slot-name>` and `Native<slot-name>` helpers also move between an exact
native view and erased storage, for example
`greeter.ValueExp_Api__pair_ret(pkg, pair)` and
`greeter.NativeExp_Api__pair_ret(pkg, value)`. All slot and host-declaration
`Value...`, `Native...`, `Prepare...`, and `Unprepare...` helpers are free
functions taking the package first. Their native host selections come before
any captured type indices in the type parameter list. These are normally inferred from
the package and typed value. A generic `Native<slot-name>` recovery may
require explicit selected indices: the erased `Value` argument cannot supply
them for Go inference. Erased storage does not establish
a new exact type: keep its contents consistent with the view you request.

When a host owns `Box[T]`, it may choose native `Box[Value]` storage. Prepare
the payload before storing it; the declared `box_get` operation returns the
stored `Value` unchanged. Use a typed native view only after that extraction.
The generated package does not look inside your box. Keep the public carrier's
type index consistent with the stored Kio value.

Preparation runs no role adapters or callbacks. The consuming package uses
its own adapters when the value enters Kio; a native view uses the instance
on which you request it. A callback already converted by another compatible
instance keeps its original captures. Private nominal payloads remain private
through `Value`, too.

Passing a prepared native view to a generic identity function still enters
Kio and performs that view's requested conversion. Relaying an already
converted `Value` or an `Atom` performs no conversion; passing a host box
does not convert or inspect its contents.

For an otherwise abstract value chosen solely by the host, use
`greeter.PrepareAtom("text")`, which has index `greeter.AtomIndex[string]`.
For erased atom storage, use `greeter.Atom("text")` and
`greeter.NativeAtom[string](value)`. This is opaque
native storage, not a way to encode a declared Kio product or nominal.
Use the exact typed slot helpers for those. A nil native interface is a valid atom
and is distinct from Unit.

## Newtypes

Every public newtype in a bridged module has its own handle under its module namespace, even
when that handle has no methods. Its public operations match the members the
package made public:

- With no public constructor or projector, values use an opaque Go type.
- With only a public constructor, the host can create values but cannot
  inspect them.
- With only a public projector, the host can inspect values received from
  the package but cannot create them.
- With both public, a non-recursive newtype keeps Go's transparent
  representation. Its ordinary constructor and projector use the payload type
  directly.

The opaque and one-member forms use a declaration-owned
`KioNewtype_...` carrier whose name encodes the complete module-and-type
identity; their storage stays private. Generic declarations carry their exact
type indices as carrier arguments. Their public operations retain exact host
bindings and declaration-owned host-type applications. A selected generic
position uses `Prepared[I]`. An
existential projector accepts a continuation instead of returning its payload
directly. Canonical Unit contributes no continuation argument; every other
payload, including a product, contributes one. This allows the host to use the hidden value without returning
its hidden type. Known declaration arguments keep their exact prepared types
inside that continuation; hidden existential arguments and its result use
`Value`. Follow its generated callback signature when supplying the
continuation. A recursive both-member newtype also uses a declaration-owned
carrier with exact type indices. Construct and inspect it through its public
members, using `CallExp_...` for generic members. An opaque or one-member
carrier inside a transparent newtype stays sealed.

Same-named newtypes in different modules therefore remain different Go types.
When an enclosing newtype contains such a carrier, it stays a single boundary
value rather than exposing its hidden payload. Normal host code reaches it
through the newtype handle's public operations and the stable `Exp_...`
slot aliases.

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
host fn print(p0: String);
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

pub fn main() { print("Hello from Kio!\n"(String)) }
```

Save this host as `main.go` beside the Kio package manifest. Create the host's
`go.mod` from [Make the package importable](#1-make-the-package-importable):

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
    pkg := greeter.New[HostString](MyHost{})
    pkg.Greeter.KioModule_main.Main()
}
```

Build the package and run the host from that directory:

```sh
kio build go
go -C out/go mod init greeter
go -C out/go mod edit -go=1.26
go run .
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

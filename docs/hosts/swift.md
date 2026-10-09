# Hosting Kio in Swift

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to Swift and
calling into it from a Swift host. The authoritative contract is
[`specs/backends/swift.md`](../../specs/backends/swift.md); this guide is the
narrative companion.

> **Known host-source compatibility caveat.** When retained signatures require
> incompatible epochs of one exact nominal declaration, Swift omits the
> affected protocol requirements and types. Existing host source must delete
> or rewrite the affected extra methods and generated-type references. See the
> Swift contract's
> [incompatible-retained-epochs section](../../specs/backends/swift.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-swift).

## The build target

A package emits to Swift by declaring a `swift` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target swift {
    out "out/swift/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build` or `kio build swift` writes a self-contained Swift module — a
single flat directory built as one Swift module (the host `import`s it):

```text
out/swift/
├── pkg.swift          // surface: the Greeter handle + createGreeter(host:) + export namespaces
├── host.swift         // the GreeterHost protocol the host conforms to
├── shapes.swift       // a Swift type per structural shape that crosses the FFI
├── ffi.swift          // stable per-slot typealiases
└── kio_runtime.swift  // fixed support and public Product/Sum/Unit types
```

The module's **name is the package's namespace**, and every top-level name
is branded from it. For `greeter` the module is `Greeter`, the handle type
`Greeter`, the host protocol `GreeterHost`, and the factory
`createGreeter(host:)`. A Swift source carries no module declaration — the
name is imposed at build time — so `pkg.swift`'s first line publishes it as
a marker:

```swift
// kio-swift-module: Greeter
```

Build the package under that name (`swiftc -module-name Greeter`, or a
SwiftPM target named `Greeter`) and `import Greeter`. The default module
name is the title-word-cased package name (`my_pkg` → `MyPkg`); affixed
source names use an exact `KioPkg_...` brand. The `namespace "<value>"`
target key overrides it (a capital-initial Swift module name), retained
exactly as both the module name and handle name.

A default name that collides with Swift, an imported stdlib module, or an
emitted support type uses the exact `KioPkg_...` frame (`swift` → `KioPkg_Swift`);
an explicit override with the
same collision is rejected. Support collisions include `KioUnit`, `Product`,
`Sum`, `KioNative`, `KioNativeConstructor`, the numeric `KioApplyN` names, and the exact generated-name classes
`KioFacade_...`, `KioHostType_...`, `KioHostTypeMk_...`, `KioNewtype_...`,
`KioNewtypeMk_...`, `KioHostTypeIdentity_...`, and `KioForall_...` names.

## How Swift hosts the package

Swift is a statically typed host, and what you see is the host-facing
**skin** — the `<Handle>Host` protocol, the shape types, the exported
signatures — which is fully typed. This matters in practice in two ways:

- A higher-kinded application crosses through an exact generic carrier, so a
  declaration-owned type constructor remains distinct from every other head.
- A boundary type variable becomes a Swift generic parameter. A rank-N value
  has an exact public callable carrier and companion implementation protocol;
  generated public signatures never ask the host to cast an `Any`.

The package body and carrier storage use `Any` internally. That erasure is an
implementation detail behind the typed surface.

Memory is **ARC** — automatic reference counting. The package body threads
no ownership annotations and the host needs none.

## Loading the package

A Swift host brings the package online in four steps.

### 1. Import the module

```swift
import Greeter
```

The module name is the one on `pkg.swift`'s marker line (`Greeter` here);
you build the package under it and import it by that name.

### 2. Conform to the GreeterHost protocol

The generated `GreeterHost` protocol in `host.swift` declares one associated
type per exact nullary `host type` and one method requirement per `host fn` the
bridged modules currently declare, plus the deprecated defaults described
below when sealed history contains a removal. A module path without source `_`
uses the module path with `/` replaced by `_`, then `__`, then the leaf. A path
containing `_` uses the exact `KioItem_<encoded-path>__<leaf>` form, word-casing
each component before escaping remaining `_` as `_u` and `/` as `_s`.
Callable leaves are word-cased in both forms (`make_pair` → `makePair`).
A `host fn print` in module `greeter` becomes the protocol
requirement `greeter__print`:

```kio {variant=module}
// greeter.kio
module greeter;

host type String role(str);

host fn print(p0: String) -> .;
```

```swift
import Greeter

struct MyHost: GreeterHost {
    typealias greeter__String = Swift.String

    func greeter__print(_ arg0: Swift.String) {
        print(arg0, terminator: "")
    }
}
```

The protocol name is the handle name plus `Host` — `GreeterHost` for the
`Greeter` module. Each exact nullary `host type` is an associated type whose
name includes its declaring module; the host conformance selects its concrete
Swift type. Here `role(str)` requires `greeter__String` to accept string
literals, and `Swift.String` is this host's choice. The role does not force
that choice: a custom `ExpressibleByStringLiteral` type is equally valid.

Swift protocol conformance is declared explicitly — the host type's
`: GreeterHost` annotation, its associated types, and its methods are the
contract. Swift may infer a used associated type from a witness signature, but
the conformance must determine every one; an explicit `typealias` is clearest
and is required in practice for an unused declaration. A mismatched method or
undetermined associated type is a compile error.

After a host method is removed, the generated protocol keeps its frozen
signature as an `@available(..., deprecated, ...)` requirement when its
nominal epochs are compatible. A deprecated
protocol-extension default traps, so a new conforming type does not implement
the removed method; an unchanged host method still satisfies the requirement.
The same deprecation marker appears on every host-visible alias, carrier,
carrier member, and structural shell kept only so that old signature still
compiles. A declaration also reached by current code stays nondeprecated. None
of this compatibility surface is connected to package dispatch, and package
code cannot call it.

If that method used a nullary host type removed at the same time, the protocol
also keeps a deprecated associated type with a constraint-correct default. A
new host chooses nothing, while Swift can still infer the old host's concrete
type from its unchanged method witness. A removed host type that no retained
method uses disappears from the protocol; an old host may leave its extra
nested `typealias` in place.

If sealed history contains incompatible declarations at one exact nominal
identity, no generated Swift type can preserve both frozen signatures. The
generator omits the affected retained requirements and type closure. Existing
host source must delete or rewrite affected extra methods and direct
generated-type references; current conformers remain live-only. See the
caveat banner above.

### 3. Instantiate the package

```swift
let pkg = createGreeter(host: MyHost())
```

`createGreeter(host:)` — `create` plus the handle name — is the sole
factory. It takes any value conforming to `GreeterHost` and returns a
`Greeter<MyHost>` handle. The handle and its export namespaces retain that
generic host parameter so exact associated types remain available throughout
the typed boundary; type inference means the call normally needs no explicit
generic arguments. All three names are used **unqualified** after
`import Greeter`; because the handle type `Greeter` equals the module name,
the import brings the type into scope and it shadows the module name — do
not write `Greeter.createGreeter`. Package construction cannot fail at the
boundary: a structural type mismatch is a compile error, not a runtime
panic.

### 4. Invoke exports

```swift
pkg.greeter.KioModule_main.main()
```

The `Greeter` handle's **export-namespace tree** keeps a natural root module
property, uses `KioModule_<exact-component>` for every nested module, and
uses `KioType_<exact-component>` for every public newtype handle. Each
exported `pub fn` is a method at the leaf. An export from a single-segment
module (`greeter`) is a method on `pkg.greeter`; an export from nested module
`greeter/main` is a method on `pkg.greeter.KioModule_main`.

Source components use word casing (`my_module` → `myModule`, `Native_token`
→ `NativeToken`), preserving initial case and outer underscore runs. Exact
selectors word-case before escaping remaining underscores as `_u`.
Function/member leaves use the same word casing (`make_pair` → `makePair`),
with `KioItem_...` escaping for affixed names and Swift escaping for keywords.
Host methods, associated-type leaves, and source-readable carrier/slot frames
also word-case their source components; their fixed role delimiters stay fixed.

## Structural values at the boundary

Keep shared tuple fields and nested Kio data unchanged while a retained
value or closure can still use them, even after a call returns. Construct a
changed tuple instead. A host-managed mutable `Box` may change through its
declared `get` and `set` host functions while the tuple keeps the same box.
See [shared data stays unchanged](../../specs/backends/README.md#10-shared-data-stays-unchanged).

Supply values matching their declared types, including values transported
through generic or opaque storage. Swift's static types and required casts are
not a recursive runtime validation guarantee. See
[host values satisfy their declared types](../../specs/backends/README.md#11-host-values-satisfy-their-declared-types).

When a host fn or an exported fn takes or returns a structural value, that
value crosses as a **typed Swift value**, not an opaque handle.

- A **product** `(A & B)` is `Product<A, B>`. Construct it with
  `Product(_0: first, _1: second)` and read its `_0` and `_1` fields.
  Wider products nest to the right: `(A & B & C)` is
  `Product<A, Product<B, C>>`. The same representation is used when a
  generic argument becomes a product.

- A **sum** `(A | B)` is the native `enum` `Sum<A, B>`. Wider sums nest to
  the right: `(A | B | C)` is `Sum<A, Sum<B, C>>`. Each case carries its
  payload as an associated value. Exact nominal carriers keep recursive
  declarations finite.
  You construct an arm by writing the case directly and distinguish the
  inhabited arm with an exhaustive `switch`:

  ```swift
  switch result {
  case ._0(let left):
      use(left)   // the first arm's payload
  case ._1(let right):
      use(right)  // the second arm's payload
  }
  ```

  This is the idiomatic Swift native encoding — an exhaustive `switch`
  with no default branch.

Stable spellings live in `ffi.swift`, which gives every boundary slot a
`typealias` — for example `Env_greeter__makePair_ret<H>` for a host fn's
exact-nullary-host-dependent return type, with `Env_..._arg0` /
`Env_..._arg1` for parameters. Use the alias at a host method boundary; the
binary structural types do not change their shape when unrelated declarations
are added. Generic values also keep that representation inside host-owned
containers; the package does not inspect or rearrange a host container's storage.

## Atomic types

Every exact nullary host type crosses as an associated type of the generated
host protocol. A role says which literal syntax it accepts, not which concrete
Swift type represents it:

- integer roles require `ExpressibleByIntegerLiteral`;
- floating roles require `ExpressibleByFloatLiteral`;
- `role(str)` requires `ExpressibleByStringLiteral`;
- `role(bool)` requires `ExpressibleByBooleanLiteral` and `Equatable`;
- a roleless host type has no extra constraint.

A Kio literal must match the exact declaration's role. An explicit annotation
or contextual expected type chooses that declaration but does not override its
role: a string literal cannot inhabit an integer-role type, and a roleless
host type accepts no lexical literal. Those mismatches are Kio compile errors,
before the generated Swift module is built.

You may bind these to familiar standard-library types (`Int32`, `Int128`,
`String`, `Bool`, and so on) or to custom types. Each declaration has its own
associated-type member, but compatible members may select the same concrete
Swift type.

A parameterized declaration such as `host type Box[A]` cannot be a Swift
associated type constructor. Its exact public type is instead
`KioHostType_<module-and-name><A>`, with a matching
`KioHostTypeMk_<module-and-name>` marker for the constructor head. The
name aliases the corresponding ordered `KioApply1` application. Its generic
initializer stores any native host value, and
`value(as:)` retrieves it at the host-selected storage type. A fully applied
host method therefore stays generic and exact without exposing `Any` in its
signature. Those construction/access methods apply only to that exact host
declaration, not an arbitrary application or a Kio newtype.

For a native constructor, use its marker's `.constructor` witness to transfer
storage: `witness.lift(value)` creates an application, and
`witness.project(application, as: Storage.self)` retrieves it. The result type
of `lift` selects the application's type argument. Use
`witness.applying(A.self)` to select an argument of a partially applied
constructor before lifting its final argument. Your own native constructor
uses `KioNative<YourMarker>.constructor`. Keep its storage representation
consistent across your host functions, just as for a fully applied host type.

`()` (unit) surfaces as the `KioUnit` type from
`kio_runtime.swift` (an empty `struct`). Swift's built-in `()` / `Void` is
awkward to carry through the erased `Any` body, so the canonical `KioUnit`
bridges that gap transparently.

## Newtypes and function values

- Every public **newtype** in a bridged module has a
  `KioType_<exact-component>` handle at its module namespace
  (`pkg.a.KioModule_b.KioType_T`), even when that handle has no methods. Values
  use an exact `KioNewtype_<module-and-name><H, ...>` carrier with private
  storage in every visibility configuration. With only the constructor
  public, the host can create but not inspect values; with only the projector
  public, it can inspect values received from the package but cannot create
  them; with both public, it can do both through the namespace methods. A
  parameterized declaration also has a `KioNewtypeMk_<module-and-name><H>` marker
  for its constructor head. Same-leaf declarations in different modules
  remain distinct types, and the nominal carrier cuts recursive graphs.

  A parameterized nominal value already has the corresponding application
  type: `KioNewtype_...<H, A>` aliases `KioApply1<KioNewtypeMk_...<H>, A>`.
  No conversion is needed. The application retains the host conformance and
  every ordered type argument, including when its payload hides another type.
  Flat `KioApply2<F, A, B>` and grouped `KioApply1<KioApply1<F, A>, B>` spellings
  are the same type. A nominal application has no raw initializer or storage
  accessor, and wrapping its marker in `KioNative` creates a different type,
  not a way to access its payload.

  Public method binders are Swift generic parameters. An existential
  projector returns an exact rank-N callable carrier instead of its payload;
  call that carrier with a continuation carrier implementing the stable
  `ffi.swift` `..._typeRet_cbarg0Implementation` alias after selecting the
  projector's result type. A continuation receives one
  argument, including `KioUnit` for a Unit payload. This lets
  the host use the hidden value while preserving its exact scoped type.

- A monomorphic **function value** is a Swift closure with one argument:
  its whole input type. A function from `A & B` receives `Product<A, B>`;
  a function from Unit receives `KioUnit`. Named host and exported methods
  still preserve their written parameter groups. Function-valued arguments
  are `@escaping`, also in higher-order closure types, so a received callback
  can be retained in another returned value.

- A polymorphic value uses a generated carrier. Call
  `value.specialize(asKioType: Int32.self)` to select one type argument and
  receive the next carrier or ordinary closure. A return-type annotation can
  infer the selection, allowing `value.specialize()` instead. Selecting a type
  is an actual stage: effects at that stage happen then, before a later
  specialization or closure invocation.
  To supply such a value, conform to its stable `...Implementation` alias and
  implement `specialize<T>(_: T.Type)`, returning the next typed value. Pass
  that implementation to the carrier initializer. The returned value can
  capture state from the selection without exposing `Any` to the host.
  Its residual type is available as the stable `<slot>_typeRet` alias; each
  later specialization extends that alias path once more.
  A callable inside a product, sum or exposed type argument adds `_fieldN`,
  `_armN` or `_typeargN` to the slot name. Indices start at zero; right-nested
  products and sums number their fields or alternatives from left to right.
  For example, a polymorphic second sum arm uses `<slot>_arm1` and
  `<slot>_arm1Implementation`. Nested structures add another segment; these
  aliases do not expose opaque payloads.

- A named method retains every declared type binder. If a binder is absent
  from its native arguments and result, select it with the trailing
  `asKioType<i>: T.self` parameter, using its zero-based head-binder index.
  A host protocol implementation declares the matching `T.Type` parameter.
  This is native generic selection, not an extra Kio value or representation
  witness; the written value-parameter groups stay unchanged.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target swift {
    out "out/swift/"
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

  target swift {
    out "out/swift/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_swift_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_swift_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() { print("Hello from Kio!\n"(String)) }
```

Build it:

```sh
kio build swift
```

Then host it in Swift:

```swift
import Greeter

struct MyHost: GreeterHost {
    typealias greeter__String = Swift.String

    func greeter__print(_ arg0: Swift.String) {
        print(arg0, terminator: "")
    }
}

let pkg = createGreeter(host: MyHost())
pkg.greeter.KioModule_main.main()
```

Running this prints `Hello from Kio!` and exits 0.

## Where this leads

- [`specs/backends/swift.md`](../../specs/backends/swift.md) — the full backend
  contract: output layout, boundary semantics, FFI surface, item naming, and
  the carve-out catalogue.
- [`specs/backends/README.md`](../../specs/backends/README.md) — cross-cutting
  properties shared by every backend, including where Swift sits in
  the language family taxonomy.
- [Getting started with the Kio tooling](../tutorials/tooling.md) — build and
  check a package before loading it in a host.
- [`specs/cli.md`](../../specs/cli.md) — the exact `kio build` command
  contract.

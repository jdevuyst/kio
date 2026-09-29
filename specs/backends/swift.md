# Swift backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `erased-static` (see
[`README.md` § Language families](README.md#language-families)).

> **Known host-source compatibility caveat.** When retained signatures require
> incompatible epochs of one exact nominal declaration, Swift omits the
> affected protocol requirements and type closure. Existing host source must
> delete or rewrite the affected extra methods and generated-type references.
> See
> [§ Incompatible retained declaration epochs are not source-stable on Swift](#incompatible-retained-declaration-epochs-are-not-source-stable-on-swift).

The `"swift"` build target ([`specs/package.md` § Build target
files](../package.md#build-target-files)) emits a self-contained Swift
module. This page describes the surface a Swift host calls against.

Swift is the **`erased-static`** family's **native-sum** member: a
statically-typed host with a typed FFI skin over an **erased body**.
Swift's `Any` is a genuine dynamic universal carrier, so the package's
internal computation flows entirely as `Any` / `[Any]` (the JS dynamic
body's shape, in Swift). The host-facing **skin** is exact: bound types are
Swift generic parameters, higher-kinded applications have generic nominal
carriers, and `Any` appears only in internal storage and adapter bodies, never
in a generated public type spelling (see [`README.md` § Higher-kinded
types](README.md#higher-kinded-types) and the note under § Language families).
Swift uses a **native `enum` with associated values**, matched by an
exhaustive `switch`, for every sum on the typed host surface.

## Language version

Emitted code targets **Swift 6.0+**.

All other constructs are standard Swift: `struct`s, `enum`s with associated
values, `switch`, closures, protocols with associated types, generics, and the
standard library only. The body is erased to `Any`; the host-facing package
handle and every boundary type that contains an exact nullary host type are
generic over the host conformance. Boundary type variables remain Swift
generic parameters. Memory is **ARC** — automatic, so the body threads no
ownership annotations.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target swift {
    out "out/swift/";
  }
}
```

`kio build swift` writes a self-contained Swift module as a flat
directory under `out` (compiled as one module — the host `import`s it):

- `pkg.swift` — the module's invocable surface: the package-handle class,
  the `create<Handle>(host:)` factory, the export-namespace tree, and
  every module fn as a method. Its **first line** is the module-name
  marker (below). **The entry point.**
- `host.swift` — the `<Handle>Host` **protocol** (one associated type per
  current exact nullary `host type`, one method per current `host fn`, plus
  the optional deprecated removal facade in § Deprecated host items). The
  host conforms to it. **Part of the surface.**
- `shapes.swift` — the generic semantic shells and exact nominal carriers the
  FFI surfaces: a `struct` per product shell, a native `enum` (with associated
  values) per sum shell, declaration-owned host/newtype carriers, application
  carriers, and callable carriers for rank-N values. **Part of the surface**
  (host code constructs, calls, and `switch`es over these).
- `ffi.swift` — stable per-slot `typealias`es naming the Swift type at every
  host-fn / export-fn boundary slot. An alias containing an exact nullary host
  type takes the host conformance as a generic argument. A host reaches a
  boundary type by its stable alias rather than reconstructing its generic
  shell or carrier spelling. **Part of the surface.**
- `kio_runtime.swift` — the fixed runtime-support file (today: the
  canonical `KioUnit`). **Internal**; rewritten on every build, not
  user-modifiable. Its content is **byte-identical across every emitted
  package** — a Swift source carries no module declaration, so nothing
  here binds the namespace (contrast Go's per-file `package <ns>` line).

### The package namespace

Every top-level name the module contributes — the handle type, the factory,
the host protocol, the shell and carrier types, the runtime-support items —
lives under one per-package **namespace**, per [`README.md`
§ The package facade](README.md#the-package-facade). Swift realizes the
namespace as the emitted **module name**.

A Swift source carries **no** module declaration: a module's name is
imposed at compile time (`swiftc -module-name <Ns>`, or the SwiftPM
target name), never written in a file. So the namespace is **published,
not declared** — `pkg.swift`'s first line is a machine-readable marker:

```swift
// kio-swift-module: <Ns>
```

This marker and this page are how a host learns the module name to build
the package under and `import`. This is a **contract by documentation**:
a Swift compiler does not read the marker, so the host's build
configuration must set `-module-name <Ns>` (or name its SwiftPM target
`<Ns>`) to match it. The corpus harness supplies that configured artifact
identity to the kio test runner independently; the runner does not inspect
emitted source to rediscover it and passes the supplied namespace to
`swiftc -module-name`.

The namespace **defaults** to the title-word-cased Kio package name
(Swift's UpperCamelCase module convention): package `greeter` → module
`Greeter`, `csv_reconcile` → `CsvReconcile`. An affixed source name uses
`KioPkg_` plus the title-word-cased component with remaining `_` escaped as `_u`
(`_my_pkg` → `KioPkg__uMyPkg`). An unmarked default uses the same `KioPkg_` frame
when it collides with a Swift reserved word (`Any`, `Self`), a stdlib module
the artifact or its host imports, or a generated top-level support name. The
support-name set is `KioUnit`, `KioNative`, `KioNativeConstructor`, the fixed `Product` / `Sum` shells, the numeric
`KioApplyN` names, and the exact generated-name classes `KioFacade_...`,
`KioHostType_...`, `KioHostTypeMk_...`, `KioHostTypeIdentity_...`, `KioNewtype_...`,
`KioNewtypeMk_...`, and `KioForall_...` names. Thus `swift` → `KioPkg_Swift`
(swiftc reserves the module name `Swift`), `foundation` → `KioPkg_Foundation`
(a `Foundation` module link-collides for a host that also imports the real
Foundation), and `kio_unit` → `KioPkg_KioUnit` (the handle would redeclare the
runtime's `KioUnit`). The exact frame keeps these defaults distinct from
ordinary source-derived names such as `swift_pkg` → `SwiftPkg`.

The optional **`namespace "<value>"` target key** ([`../package.md`
§ Per-target keys](../package.md#per-target-keys)) overrides the default.
Its grammar is `[A-Z][A-Za-z0-9_]*` — a capital initial (matching the
default derivation and Swift's module convention), then letters, digits,
and underscores; a reserved word, a shadowing stdlib module, or any exact
generated support name from the set above is rejected. The explicit namespace
is retained exactly, including underscores; it is not word-cased a second time.

### Branded names

The facade's public handle name is the effective namespace exactly — call it
`<Handle>` — so one root names the whole surface and a
host reconstructs it from the marker alone:

| Surface | Name | For module `Greeter` |
| --- | --- | --- |
| Package handle type | `<Handle>` | `Greeter` |
| Host contract protocol | `<Handle>Host` | `GreeterHost` |
| Factory | `create<Handle>(host:)` | `createGreeter(host:)` |

The handle and its export-namespace classes take the concrete host conformance
as `H`, so the instantiated type is `Greeter<MyHost>`; the branded type name
remains `Greeter`, and the factory infers `H` from its argument.

For a single-segment namespace the handle type name **equals** the module
name (both `Greeter`). This is deliberate and compiles cleanly: a host
`import Greeter` sees the type `Greeter`, which shadows the module name in
the host's own scope, so `createGreeter`, `GreeterHost`, and the returned
`Greeter` value all resolve **unqualified**. (A host must not write
`Greeter.createGreeter` — there `Greeter` binds the *type*, which has no
such member.) The FFI surface is `public`; the erased body is internal.

Two packages with distinct namespaces load into one host program without
symbol conflict: no emitted type, factory, or runtime-support item has a
fixed top-level name, and each package is its own Swift module ([`README.md`
§ The package facade](README.md#the-package-facade) § Coexistence).

## Loading protocol

Per [`README.md`](README.md) (synchronous, in-process). A Swift host,
having built the package under its module name `<Ns>` (read from the
`pkg.swift` marker — § Output layout):

1. `import <Ns>` (e.g. `import Greeter`);
2. supplies a value conforming to the `<Handle>Host` protocol (§ Host
   record contract);
3. calls `create<Handle>(host:)` to obtain a `<Handle>` handle specialized to
   that host conformance — all
   unqualified, since the import brings the module's public names into
   scope (§ Output layout § Branded names);
4. invokes exported items through the handle's namespace properties
   (§ Package API).

Package instantiation has no failure mode at the boundary: a structural
type mismatch between the host and the emitted protocol is a Swift
**compile error**, caught when the host module is built — the
statically-typed interface is the contract.

## Package API

Exported items (`pub fn` in a bridged module) are reached through the
`<Handle>` handle's **export-namespace tree**. A root module keeps its
word-cased spelling followed by Swift keyword escaping. Every nested module uses
`KioModule_<exact-component>`, with word casing before escaping remaining `_` as `_u`; a
`pub fn` in module `a/b` is therefore reached at
`pkg.a.KioModule_b.fn()`. A `pub fn` in a
single-segment module `m` is a method on the `pkg.m` namespace property
(`pkg.m.fn()`): entries keep their module namespace down to a single
segment ([`specs/package.md` § The bridge
block](../package.md#the-bridge-block)). A multi-value-group export
takes every group's parameters flat in one call — the method applies
the internal curried layers group by group
([`README.md` § Function-type FFI canonicalization](README.md#function-type-ffi-canonicalization)).

An exported method's signature is the host's typed contract: each value
parameter is its boundary shape (§ FFI surface), and the return is its
boundary shape (`KioUnit` for a `()` return). The method converts each
typed argument into the internal representation, runs the package
function, and converts the result back to the typed shape — the host
never sees the internal `[Any]` rep.

Every exported **`pub newtype`** contributes a **per-type handle** under
its module namespace, even when it exports no members. A type handle uses
the disjoint exact selector `KioType_<exact-component>`; a newtype `T` in
module `a/b` is reached at `pkg.a.KioModule_b.KioType_T`. The handle exposes
exactly the constructor and projector whose declarations are `pub`. The four
surfaces are:

| Public members | Swift boundary value |
| --- | --- |
| neither | an exact declaration-owned carrier |
| constructor only | the carrier, constructible only through that method |
| projector only | the carrier, inspectable only through that method |
| both | the carrier, constructible and inspectable through those methods |

Every public newtype uses a public nominal
`KioNewtype_<exact-module-and-name><H, ...>` carrier with internal storage.
Its available members have the exact declared Kio member schemes after the
Swift boundary mapping; generic positions retain their mapped types. An
existential projector returns the exact rank-N callable carrier for its CPS
result, and the host supplies the continuation through that carrier's `call`
method rather than receiving the hidden type. Canonical Unit contributes zero
continuation value arguments, and every other payload contributes one. A
parameterized newtype also publishes a
declaration-owned `KioNewtypeMk_<exact-module-and-name><H>` constructor marker so
partial kind application retains the nominal head rather than collapsing to a
role or representation type.

For each parameterized public newtype, the package handle exposes
`KioNewtypeApplication_<exact-module-and-name>_lift` and the matching `_project`.
Their generic parameters are the declaration's ordered type arguments. `lift`
maps its exact `KioNewtype_...<H, ...>` carrier to
`KioApplyN<KioNewtypeMk_...<H>, ...>`; `project` maps that application back.
Both preserve the existing value, including existential payloads, without
constructing or exposing its payload. They are available regardless of member
visibility and grant neither a constructor nor a projector. The exact
declaration, host conformance and every type argument remain part of both
types; values cannot be rebranded across those identities.

Carrier identity is the exact declaring module plus newtype name. Two
same-leaf declarations in different modules remain different Swift types. A
carrier is an atomic boundary leaf: its hidden payload is not walked while
deriving an enclosing shell. Thus recursive and mutually recursive nominal
graphs remain finite without exposing an erased public fallback.

Root modules and ordinary function methods keep readable Swift spellings,
while nested modules and type handles occupy the disjoint `KioModule_...` and
`KioType_...` selector classes. A function, nested module, and public newtype
handle can therefore share a source stem without a precedence rule or dropped
entry.

There is no separate generic intrinsic API for constructing opaque
structural values: a structural value crosses the boundary as its
**named Swift shape** (a `struct` / `enum` value the host builds and
reads directly), not as an opaque handle. See § FFI surface.

## Host record contract

Each `host fn` declared in a bridged module becomes one requirement on
the `<Handle>Host` **protocol** in `host.swift` (`GreeterHost` for a
module `Greeter`). The host supplies a conforming type.

A method whose declaring module path contains no source `_` uses the
slash-path with `/` → `_`, then `__`, then the fn leaf. Thus `host fn print`
in module `app/io` is `app_io__print`, and `host fn make_pair` in module
`greeter` is `greeter__makePair`. A path containing `_` instead uses
`KioItem_<exact-path>__<leaf>`, with word casing before escaping remaining
`_` as `_u` and `/` as `_s`. Callable leaves are word-cased in both forms.
The mapping is injective across modules.

Each exact nullary `host type T` declared in module `m` becomes an
`associatedtype` requirement on `<Handle>Host`. Its boundary identity includes
the declaring module: paths without `_` use `/` → `_` followed by `__T`
(`app/io.T` → `app_io__T`). A path containing `_` uses the reserved
`__kio_host_` prefix and word-cases components before encoding remaining `_`
as `_u` and `/` as `_s`, so legal source identifiers cannot make two
module/type pairs collide. The type leaf is word-cased (`Native_token` → `NativeToken`).

A parameterized `host type F[A, ...]` is not a Swift protocol associated type:
Swift protocols cannot express an associated type constructor directly. It
instead has a declaration-owned generic carrier
`KioHostType_<exact-module-and-name><T0, ...>` and a zero-case constructor
marker `KioHostTypeMk_<exact-module-and-name>`. This marker is an alias of
`KioNative<KioHostTypeIdentity_<exact-module-and-name>>`. A fully applied occurrence uses
the carrier. A partial or abstract application uses the marker as its exact
head and `KioApplyN<F, T0, ...>` as the generic application carrier. The fully
applied `KioHostType_...` carrier's public generic initializer and `value(as:)` accessor let a host
choose its native storage without exposing `Any` in the public signature.

Abstract application carriers are opaque: `KioApply1<F, A>` has no public
initializer or storage accessor. `KioApplyN<F, A, ...>` is an alias of the
ordered nested unary applications, so flat and grouped application spellings
retain the same complete identity, including a partially applied nominal's H.

Native application storage is accessed through the closed
`KioNativeConstructor<F>` witness. `KioNative<Root>.constructor` supplies the
root witness; a generated host constructor marker exposes the same property.
`witness.applying(A.self)` derives the witness for `KioApply1<F, A>`.
`witness.lift(value)` produces `KioApply1<F, A>` at the result's selected A;
`witness.project(application, as: Storage.self)` retrieves its native storage.
Witnesses cannot be directly initialized by a client. An unconstrained F or a
nominal marker supplies no witness. Wrapping a nominal marker in `KioNative`
creates a distinct native constructor; it cannot access values belonging to
the original nominal constructor. Native storage consistency remains the
host's responsibility, as with fully applied host carriers.

A role constrains which literals the associated type accepts; it does not
choose the host representation or equate the associated type with a Swift
primitive:

| Kio declaration | Swift associated-type constraint |
| --- | --- |
| `role(i8)` … `role(i128)`, `role(u8)` … `role(u128)` | `ExpressibleByIntegerLiteral` |
| `role(f32)`, `role(f64)` | `ExpressibleByFloatLiteral` |
| `role(str)` | `ExpressibleByStringLiteral` |
| `role(bool)` | `ExpressibleByBooleanLiteral, Equatable` |
| no role | none |

Kio accepts a lexical literal at that exact host type only when the
declaration's role matches the literal class. An explicit annotation or an
expected type selects the exact declaration; it does not bypass the role
check. A roleless host type therefore accepts no lexical literal, and a
mismatched literal is rejected before Swift emission.

`Equatable` is required only because emitted `if` / `else` code compares an
exact Boolean value with that associated type's `true` literal. A host chooses
the concrete type in its conformance—for example
`typealias app__String = Swift.String`—and may choose a custom type with a
nonstandard literal carrier. Kio's role declaration makes no further Swift
representation promise.

Each host-fn value parameter and return takes its boundary type (§ FFI
surface): an exact nullary host atom as `Self.<associatedtype>`, a parameterized
host atom as its declaration-owned carrier, a structural compound as its
generic semantic shell, and `()` as `KioUnit`. Callable type stages become
Swift method generic parameters; value parameters follow the prepared source
layout exactly.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) for
Swift.

Swift re-emits a removed `host fn` whose frozen closure has no incompatible
nominal epoch as an
`@available(*, deprecated, message: ...)` protocol requirement with an equally
deprecated protocol-extension default that unconditionally traps. The method's
exact frozen signature comes from sealed signature history. An unchanged host
method remains its witness, while a host written against the current package
omits it and receives the default. The retained requirement and default never
enter the package's host adapter, loader match, capability set, or dispatch;
package code cannot call them.

Every emitted declaration whose only use is that frozen signature is likewise marked
`@available(*, deprecated, message: ...)`: stable `ffi.swift` aliases and the
exact structural, parameterized-host, public-newtype, application, and rank-N
carriers in `shapes.swift`. A declaration reached by a live site remains live
and is not deprecated.

When a retained method names a removed nullary `host type`, the protocol keeps
its associated type as a deprecated requirement with a concrete default. The
default is `Swift.Int` for an integer role, `Swift.Double` for a floating role,
`Swift.String` for `role(str)`, `Swift.Bool` for `role(bool)`, and `KioUnit` for
a roleless type, so every role constraint is satisfied without a choice from a
current host. An unchanged method witness may still infer a different concrete
associated type from its frozen signature. A removed nullary host type that no
retained method reaches needs no protocol member: an old host's now-extra
nested `typealias` remains valid Swift source.

This is Swift's source-stable realization of a compatible env-side removal
(see [`../versioning.md` § The compatibility
relation](../versioning.md#the-compatibility-relation)). It does not turn any
removed function or type into a current host obligation.

### Incompatible retained declaration epochs are not source-stable on Swift

One generated Swift nominal identity cannot describe two incompatible exact
declaration epochs. Under the shared
[`README.md` rule](README.md#incompatible-retained-declaration-epochs), Swift
omits every retained protocol requirement, default, carrier, and support
declaration that depends on the conflict. An extra old method is harmless only
while its complete signature remains nameable; affected host source must
delete or rewrite that method and any reference to an omitted generated type.
Current conformers still implement and select only live declarations.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs); its
comment cites the shared rule's explicit backend fan-out to this subsection.

Swift remains **addition**-breaking, exactly as the cross-backend idiom notes: a
new `host fn` or exact nullary `host type` adds a protocol requirement, so a
host type that does not implement it no longer conforms — a Swift **compile
error** at the host call site. The contravariant asymmetry — removals tolerated,
additions breaking — holds on Swift as on every backend; only the
*removal*-side retention is backend-specific, and the Rust backend's
`#[deprecated]` re-emit
([`rust.md` § Deprecated host items](rust.md#deprecated-host-items)) is Rust's
corresponding host-language realization.

## Boundary semantics

Each cross-cutting property is inherited from [`README.md`](README.md);
the Swift instantiations:

- **Synchronous calling convention** ([`README.md`](README.md)) — a host
  call and an exported invocation are each a direct Swift method call.
  No async, no scheduler.
- **Type erasure at the FFI** ([`README.md`](README.md)) — the body is
  erased to `Any`; the *skin* is fully typed. `Any` is confined to internal
  storage and adapter bodies. A boundary binder becomes a Swift generic type
  parameter, a higher-kinded application uses an exact generic carrier, and a
  rank-N value uses an exact callable carrier. Internal polymorphic callables
  retain the ordered stages specified by [`README.md` § 2](README.md#2-type-erasure-at-the-ffi):
  every binder of an escaped value is one hidden callable stage, while a
  direct known call may compact adjacent leading type applications. The skin
  realizes the same order through Swift generic methods and typed value
  parameters.
- **Facade topology and execution provenance** ([`README.md`](README.md#facade-topology-and-execution-provenance))
  — the package execution plan and package facade contain current callable
  sites only. A removed host function may leave a deprecated,
  defaulted protocol requirement plus its exact frozen type dependencies for
  source compatibility, but it leaves no package dispatch entry, live adapter,
  loader requirement, or executable package route.
- **Open-world property** ([`README.md`](README.md)) — adding a declaration
  to a module body never changes the meaning of existing emitted code. A
  shell name is a pure encoding of its structural kind and ordered semantic
  keys; each nominal carrier or member is a pure encoding of its exact
  qualified declaration; and each callable carrier is a pure encoding of its
  exact prepared site and use. Types, slots, and conversions consume those
  identities and the live prepared catalog directly. They do not depend on
  discovery order, a collision registry, or unrelated declarations.
- **Package isolation**, **exception propagation**, **well-foundedness
  inheritance**, **behavioral additivity** ([`README.md`](README.md)) —
  inherited unchanged. A Kio `__absurd__` / bottom-typed dead end emits a
  Swift `fatalError`; a host `exit(n)` propagates through the process
  exit code.

## FFI surface

### Atomic types

An exact nullary host type crosses as the corresponding `<Handle>Host`
associated type. Its role supplies only the literal-protocol constraint in
§ Host record contract; the host's conformance selects the concrete Swift
type. Consequently two Kio declarations with the same role—or even the same
leaf name in different modules—remain distinct associated types and may have
different Swift representations. Matching-role literals are constructed
directly at that exact associated type; roleless and role-mismatched literals
are rejected by Kio typing.

`()` (unit) is the canonical `KioUnit` (an empty `struct` — Swift `()` /
`Void` is awkward to carry nominally through the erased `Any` body).

### Structural and nominal types

The skin consumes the shared prepared boundary transaction
([`README.md` § Structural FFI shape conventions](README.md)). A compound
type's Swift realization is:

- **Product** `(A & B & C)` → a generic `struct` shell with one field per
  semantic slot. The common anonymous binary product is
  `Product<T0, T1>`; every other shell has an injective
  `KioFacade_V1_Product_...<T0, ...>` name derived from its ordered semantic
  keys. Each concrete payload type is a generic argument, never part of the
  shell name. Field names use the shared semantic key: bare newtype name,
  exact module-qualified name when needed, or positional `_<i>`.
- **Sum** `(A | B | C)` → a generic native `enum` shell. The common anonymous
  binary sum is `Sum<T0, T1>`; every other shell has an injective
  `KioFacade_V1_Sum_...<T0, ...>` name from its ordered semantic keys. Each
  case uses the corresponding semantic key and carries that arm's generic
  payload as an associated value. The host distinguishes the inhabited arm
  with an exhaustive `switch`. This is the idiomatic native encoding — Swift
  is the `erased-static` family's native-sum member, realizing the family's
  *first-class enums/sealed types* trait natively (see [`README.md` § Language
  families](README.md#language-families)).
- **Newtype** → the exact
  `KioNewtype_<module-and-name><H, T0, ...>` carrier. Visibility controls which
  namespace methods can construct or project it; it never changes the
  carrier's identity. The carrier cuts recursive nominal graphs at the public
  boundary.
- **Function value** → a Swift closure `(...) -> ...` whose compound legs are
  converted across the boundary. Its parameters use the exact prepared source
  layout, while the erased body adapter uses the exact execution layout.
- **Type variable / abstract application** → the corresponding Swift generic
  type parameter, or `KioApplyN<F, ...>` when applying an abstract head.
- **Rank-N value** → an exact `KioForall_<site-and-use><H, ...>` carrier with a
  public generic `call` method. Its companion `...Implementation` protocol
  lets a host construct a polymorphic value without an erased public
  signature. Optional metatype witnesses on `call` select quantified types
  that do not occur in value arguments.

The `ffi.swift` `typealias`es name each boundary slot's Swift type stably
(`Env_<member>_arg<i>`, `Env_<member>_ret`, `Exp_<member>_…`, a function
param's legs as `…_cbarg<j>` / `…_cbret`) so a host references a realization
by alias. An alias whose type contains an exact nullary host type is generic,
for example `Env_app__makePair_ret<H: GreetHost>`. A public newtype member's
`Exp_` name retains the module, newtype, and member as separate components;
it never pre-concatenates source identifiers. A nested rank-N callable also
publishes `<slot>Implementation` as a stable alias for its companion protocol,
so host code can construct the callable without reconstructing its encoded
site/use name.

### Carve-outs

None. Swift expresses every spec- and IR-admitted FFI shape its host
language can carry: structural products as `struct`s, **sums as native
`enum`s with associated values** (exhaustive
`switch`), exact host-selected nullary atoms, function values as closures, and
the erased higher-kinded body.

## Item naming

At the FFI surface:

Swift uses [shared source-component word casing](README.md#source-derived-public-names)
(`do_work` → `doWork`, `Native_token` → `NativeToken`) before exact escaping.

- **Top-level branded names** — the module name `<Ns>` (the marker's
  value), the handle type `<Handle>` (that exact value), the host protocol
  `<Handle>Host`, and the factory `create<Handle>` (§ Output layout
  § Branded names).
- **Host protocol methods** — the readable `<module-with-_>__<leaf>` form for
  paths without source `_`, and exact `KioItem_<encoded-path>__<leaf>`
  otherwise (§ Host record contract).
- **Host protocol associated types** — the exact collision-proof module/type
  spelling from § Host record contract (`app_io__T` for paths without source
  underscores, the reserved `__kio_host_…__T` encoding otherwise). Only
  nullary host declarations use associated types.
- **Exported items** — a root module uses word casing; nested modules
  use `KioModule_<exact-component>`; public newtype handles use
  `KioType_<exact-component>`. Conventional snake-case function/member names
  use word casing; affixed function/member names use an exact
  `KioItem_<encoded-component>` selector.
- **Nominal carriers** — parameterized host types use
  `KioHostType_<exact-module-and-name>` and
  `KioHostTypeMk_<exact-module-and-name>`; public newtypes use
  `KioNewtype_<exact-module-and-name>` and, when parameterized,
  `KioNewtypeMk_<exact-module-and-name><H>`. Same-leaf declarations in different
  modules never share a carrier or marker. Their source-readable components
  use word casing and exact escaping, without changing nominal identity.
  Abstract applications use
  `KioApply<arity>`.
- **Structural shells** — anonymous binary shells are `Product` and `Sum`.
  Every other shell uses the reversible
  `KioFacade_V1_<Product-or-Sum>_...` codec over its kind, arity, and ordered
  bare / qualified / positional semantic keys. Payload types are generic
  arguments and cannot rename the shell. Fields and cases use the same
  semantic keys. Public source-named fields and cases apply word casing;
  the opaque shell identity codec retains the raw semantic keys.
- **Rank-N carriers** — `KioForall_<exact-site-and-use>`; the companion
  protocol appends `Implementation`. Source components in the site frame
  word-case before exact escaping; fixed role and use indices remain fixed.
  The exact prepared site owner and facade
  use determine the name without a package-wide collision allocator.
- **`ffi.swift` aliases** — `Env_<member>_<slot>` / `Exp_<member>_<slot>`.
  An `Env_` member uses the corresponding host-protocol method spelling;
  an ordinary-function `Exp_` member uses the exported module-and-function
  spelling. A public-newtype-member `Exp_` member uses the exact module
  spelling followed by
  `____KioType_<encoded-newtype>__KioItem_<encoded-member>`: the ordinary
  module/member separator plus a reserved role frame whose newtype and member
  components independently word-case before encoding remaining `_` as `_u`.
  Thus `(A_b, c)` and `(A, b_c)`
  remain distinct. An alias containing an exact host atom takes
  `<H: <Handle>Host>`.

Field keys follow the 3-step key fallback ([`README.md`](README.md)); the
positional fallback is `_<i>`.

## Worked example

A package `greet` with a host `print` and an exported `main`:

```kio
// greet.pkg.kio
package greet;
build { cache "out/.kio-cache/"; target swift { out "out/swift/"; } }
bridge { app; app/**; }
```

```kio
// app.kio
module app;
host type Str role(str);
host fn print(s: Str) -> .;
```

```kio
// app/main.kio
module app/main;
import app(Str);
import app(print);
pub fn main() -> . { print("hello\n"(Str)) }
```

`kio build swift` writes `out/swift/{pkg.swift, host.swift,
shapes.swift, ffi.swift, kio_runtime.swift}`. The package name `greet`
derives the module name `Greet`, published on `pkg.swift`'s first line as
`// kio-swift-module: Greet`; the host builds the package under
`-module-name Greet` and:

```swift
import Greet

struct MyHost: GreetHost {
    typealias app__Str = String

    func app__print(_ s: String) { print(s, terminator: "") }
}

let pkg = createGreet(host: MyHost())
pkg.app.KioModule_main.main()
```

This prints `hello` and exits 0.

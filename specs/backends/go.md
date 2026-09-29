# Go backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

> **Known host-source compatibility caveat.** Removing a nullary bridged
> `host type` removes its package-root generic argument, so an existing host
> must drop that type selection from every package-root-generic spelling,
> including `<Handle>Host[...]`, `<Handle>[...]`, `Create<Handle>[...]`, and
> any `Env_` / `Exp_` alias it names. Go has no optional or default type
> argument. See
> [§ Host-type removal cannot preserve generic arity](#host-type-removal-cannot-preserve-generic-arity).
> When retained signatures require incompatible epochs of one exact nominal
> declaration, Go also omits the affected aliases and type closure; existing
> host source must delete or rewrite method signatures and other references
> that name them. See
> [§ Incompatible retained declaration epochs are not source-stable on Go](#incompatible-retained-declaration-epochs-are-not-source-stable-on-go).

**Family:** `erased-static` (see
[`README.md` § Language families](README.md#language-families)).

The `"go"` build target ([`specs/package.md` § Build target
files](../package.md#build-target-files)) emits a self-contained Go
package. This page describes the surface a Go host calls against.

Go is the **`erased-static`** family's first member: a statically-typed
host with a typed FFI skin over an **erased body**. Go's `interface{}`
is a genuine dynamic universal carrier, so the package's internal
computation flows entirely as `interface{}` / `[]any` (the JS dynamic
body's shape, in Go). Higher-kinded carriers ride that uniform rep with
**no carrier-walk** — a kind-`*→*` carrier `F(A)` is an ordinary erased
value, so the higher-kinded re-key is implicit (see [`README.md`
§ Higher-kinded types](README.md#higher-kinded-types) and the note under
§ Language families). Only the host-facing **skin** is typed.

## Language version

Emitted code and consuming host modules target **Go 1.26+**. The selected
stable line provides:

- The predeclared **`any`** alias for `interface{}` (Go 1.18). The
  emitted body and skin use `any` throughout for the universal erased
  value.
- **Generic type aliases**, finalized and enabled by default since Go 1.24.
  The Go 1.26 floor includes this host-facing type facility without an
  experiment flag.

A module that builds the emitted package declares `go 1.26`. No
`toolchain` directive or `GOEXPERIMENT` setting is required. The erased
body itself remains non-generic: its universal representation is `any`,
independently of the typed host-facing declarations.

`i128` / `u128` map to `*math/big.Int` (standard library); see
§ FFI surface > Atomic types.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target go {
    out "out/go/";
  }
}
```

`kio build go` writes a self-contained Go package as a flat directory
under `out` (a Go package is one directory). Every file declares the
package's **namespace** as its `package` clause: the kio package name by
default (a collision with a Go keyword or with an unimportable Go
package name — `main`, the program package, or the import-restricted
`internal` — uses `__kio_pkg_<escaped-source>`, with `_` escaped as `_u`,
so the default is always importable),
overridden by the `namespace` build-block key
([`specs/package.md` § Per-target keys](../package.md#per-target-keys)).
A default whose branded handle would land on a fixed package type — `Unit`,
`Product`, `Sum`, or `KioSum` — uses that same exact default namespace
(`unit` → `__kio_pkg_unit`, handle `KioPkg_Unit`). These names stay reserved even when the current
boundary does not reach the corresponding shell, so adding an unrelated
declaration cannot introduce a later handle/type collision. An
explicit value must match `[a-z_][a-z0-9_]*`, be neither a Go keyword nor one
of those unimportable names, and derive a non-empty handle outside that exact
four-name set; anything else is a build error. Near misses such as `united`,
`product_row`, and `sum_k0_case` remain valid. The facade's branded names all
derive from the namespace: for `<pkg>` = `greeter`, the handle is `Greeter`
(the namespace's title word casing), the host contract `GreeterHost`, and
the factory `CreateGreeter`. Ordinary source-shaped namespaces keep their
artifact spelling (`my_pkg`) while the public brand joins words (`MyPkg`).
Affixed source names and reserved defaults use `KioPkg_...`; other explicit
namespaces use `KioNs_<hex-UTF-8>`, as in [the shared brand encoding](README.md#branded-naming).

- `pkg.go` — the package's invocable surface: the generic `Greeter[...]`
  handle, the `CreateGreeter[...](host)` factory, the export-namespace tree, and every
  module fn as a method. **The entry point.**
- `host.go` — the generic `GreeterHost[...]` interface (one method per
  `host fn`, plus declaration-keyed role conversion methods). The host selects
  every exact host-type binding and implements the resulting interface.
  **Part of the surface.**
- `shapes.go` — the reachable generic product shells, sum-row declarations,
  row-anchored `KioSum` support, and concrete nominal carriers. Payload
  types are generic arguments rather than separate payload-specific
  declarations. **Part of the surface** (host code constructs products and
  matches sums through the stable aliases in `ffi.go`).
- `ffi.go` — one stable type alias for every compound, concrete-primitive,
  erased, Unit, and function-valued parameter or return at every live host-fn /
  export-fn / public-newtype-member site and every retained host-fn site.
  A direct exact nullary host binding uses the package-root type parameter
  itself. Function aliases
  recurse through `_cbarg<i>` / `_cbret`, products through `_field<i>`, and
  sum payloads through `_<i>_value`; each encountered sum alias also has one
  typed `<alias>_<i>` case alias and `New<alias>_<i>(value)` constructor per
  arm. **Part of the surface.**
- `kio_runtime.go` — the runtime-support file containing the canonical
  `Unit`, with canonical content below its `package` clause. **Internal**;
  rewritten on every build, not user-modifiable. Named without a leading
  `_` because the Go toolchain ignores `_`-prefixed source files.

The host authored the package's manifest, so it knows the namespace
without reading build output; the derivation above makes every branded
name reconstructible from the `package` clause alone. Fixed filenames and
support types remain scoped by Go's package/import qualification, so two Kio
packages with distinct namespaces coexist in one Go build.

## Loading protocol

Per [`README.md`](README.md) (synchronous, in-process). A Go host (for
a package `greeter`):

1. imports the emitted package (import path host-chosen — a `replace`
   directive, vendoring, or a local module path; the package qualifies
   as its namespace, `greeter`);
2. selects one Go type for every exact host-type binding and supplies a value
   implementing the resulting `GreeterHost[...]` interface (§ Host record
   contract);
3. calls `greeter.CreateGreeter[...](host)` to obtain a `*Greeter[...]`;
4. invokes exported items through the package's namespace fields
   (§ Package API).

Package instantiation has no failure mode at the boundary: a structural
type mismatch between the host and the emitted interface is a Go
**compile error**, caught when the host package is built — the
statically-typed interface is the contract.

## Package API

Exported items (`pub fn` in a bridged module) are reached through the
package handle's **export-namespace tree**. A multi-value-group export
takes every group's parameters flat in one call — the method applies
the internal curried layers group by group
([`README.md` § Function-type FFI canonicalization](README.md#function-type-ffi-canonicalization)).
Items are reached: for a package `greeter`, a
`pub fn` in module `a/b` is reached on the `*Greeter` handle at
`pkg.A.KioModule_b.<Fn>()`. A root module segment keeps the natural exported
component spelling: unmarked names use title word casing, while a leading
or trailing underscore run uses `KioItem_<encoded-component>`. The encoded
component word-cases first, then escapes remaining `_` as `_u`; thus root
module `foo_bar` is `FooBar`, while `_foo_bar` is
`KioItem__ufooBar`. Every module segment below that root instead uses the
disjoint exact class `KioModule_<encoded-component>`; for example, nested
segment `foo_bar` is `KioModule_fooBar`. Function method names use the same
natural/exact component rule as root modules. A `pub fn` in a single-segment
module `m` is a method on the
`pkg.M` namespace field (`pkg.M.<Fn>()`): entries keep their module
namespace down to a single segment ([`specs/package.md` § The bridge
block](../package.md#the-bridge-block)).

An exported method's signature is the host's typed contract: each exposed
positional slot is its boundary shape (§ FFI surface), and the return is its
boundary shape (`Unit` for a `()` return). A canonical Unit-domain layer adds
no parameter; Unit that occupies an already-planned slot maps to `Unit`. The
method converts each typed argument into the internal representation, runs the
package function, and converts the result back to the typed shape — the host
never sees the internal `[]any` rep.

Every exported **`pub newtype`** contributes a **per-type handle** under
its module namespace, even when it exports no members: a newtype `T` in
module `a/b` is reached at
`pkg.A.KioModule_b.KioType_T`. Every handle uses the disjoint exact class
`KioType_<encoded-component>` (`Foo_bar` becomes `KioType_FooBar`). The
handle exposes exactly the constructor and projector whose declarations are
`pub`; each member name uses the same natural/exact exported-component
rendering as an ordinary `pub fn`. Module selectors, type-handle selectors,
and function/member method names therefore occupy disjoint Go name classes;
same-stem declarations in Kio's separate namespaces all remain available. The
four surfaces are:

| Public members | Go boundary value |
| --- | --- |
| neither | a declaration-specific opaque carrier |
| constructor only | the carrier, constructible only through that method |
| projector only | the carrier, inspectable only through that method |
| both | the established transparent payload representation when finite |

An opaque or one-member carrier is one exported, concrete, non-generic Go
`struct` with hidden storage. Its name is branded from the complete qualified
Kio declaration identity (`KioNewtype_V1_…`; § Item naming), so a same-leaf
newtype in another module is a different Go type without relying on package
occupancy. Its selected member has the exact declared Kio member scheme after
the Go boundary mapping, including exact host-type applications, and an
existential projector takes a continuation rather than returning the hidden
type. With both members public and no existential
binders, the constructor (`payload → T`) and projector (`T → payload`)
remain identities at the boundary and use the payload's boundary shape
unchanged. An existential projector keeps its CPS signature: canonical Unit
contributes zero continuation value arguments, a product contributes one per
flat right-spine slot, and every other payload contributes one. A both-member
recursion that is not already cut by an opaque or one-member carrier keeps the
erased fallback described under § FFI surface.

Carrier identity is the exact declaring module plus newtype name. Two
same-leaf declarations in different modules remain different Go types.
A carrier is an atomic boundary leaf: its hidden payload is not walked
while deriving an enclosing shape. Thus a transparent `Outer` whose
payload is an opaque `Hidden` contains the `Hidden` carrier rather than
reopening its payload, including when the two declarations refer to one
another.

There is no separate intrinsic API for structural values. A product crosses
as a concrete application of a generic Go `struct` shell; a sum crosses as a
concrete `KioSum[Row]`. The stable boundary aliases name those applications
and supply the sum's typed cases and constructors. See § FFI surface.

## Host record contract

Each `host fn` declared in a bridged module becomes one method on the
host-contract **interface** in `host.go` (`GreeterHost` for a package
`greeter` — the handle name + `Host`). The host supplies an
implementation; Go interface satisfaction is structural, so any value
with the right method set satisfies it.

A method whose declaring module contains no `_` uses the module's slash-path
with `/` → `_`, then `__`, then the fn leaf, with the **first character
capitalized** so the method is **Go-exported**; the leaf uses word casing.
Thus `host fn print` in module
`app/io` is `App_io__print`. When the module path contains `_`, the method is
`KioItem_<encoded-module>__<leaf>`, with word casing before escaping remaining
`_` as `_u` and `/` as `_s`; `data_api.do_work` becomes
`KioItem_dataApi__doWork`. The internal capitals
in `KioItem_` keep this fallback disjoint from the readable form. Both forms
are injective across declaring-module/leaf pairs.

Every live host-type declaration contributes an exact declaration-keyed
binding. Live nullary bindings are type parameters on `GreeterHost[...]`,
`Greeter[...]`, every generated namespace and newtype handle that carries
them, and `CreateGreeter[...]`. Their order is the generated declaration's
exact qualified-name order. Two live Kio declarations remain two independent
parameters even when both have the same role or the host selects the same
underlying Go representation. History-only declarations never add a current
host type parameter or adapter method.

Each exposed positional slot takes its boundary shape (§ FFI surface): an
exact nullary host binding uses its selected type, a parameterized host type
uses its declaration-owned carrier application, a structural compound uses
its `shapes.go` type, and Unit that occupies an already-planned slot uses
`Unit`. A canonical Unit-domain declaration adds no parameter. The return
takes its boundary shape except that a `()`-returning method has no return
clause.

Following the shared [declaration-keyed conversion-adapter
rule](README.md#declaration-keyed-conversion-adapters), for each live nullary
`host type T role(r)`, the interface additionally declares
the declaration-keyed methods `KioHostIn_<identity>(T) <role-type>` and
`KioHostOut_<identity>(<role-type>) T`. These are the only conversion between the
host-selected public type and the erased body's private role representation.
Selecting the role's ordinary Go type makes them identity methods; selecting a
distinct named Go type keeps that public identity everywhere without changing
the body's numeric, string, or other role operation. A roleless nullary host
type needs no conversion methods and crosses the skin as its selected type.

Apart from those declaration-keyed role conversions, the host-contract
interface declares exactly the package's `host fn` items. It is not a
value-layout table, and a roleless host type contributes no method of its own.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) for Go.

The Go backend is **removal-tolerant at the method boundary**. Go interface
satisfaction is structural: a host value satisfies the host-contract
interface as long as its method set is a superset of the interface's
([§ Host record contract](#host-record-contract)). Removing a `host fn` is a
compatible env-side change at the language level (see
[`../versioning.md` § The compatibility
relation](../versioning.md#the-compatibility-relation)); the regenerated
interface declares one fewer method, and a host that still implements that
method satisfies the smaller interface because the extra method is ignored.
The removed method itself is never re-emitted.

The old method's source may name generated boundary types, so for a
representable frozen closure `ffi.go` keeps its stable
`Env_<member>_arg<i>` / `Env_<member>_ret` alias tree from the version-exact
signature. `shapes.go` likewise keeps only the generic
shells and concrete nominal carriers reachable from that signature. These are
plan-only type dependencies: they contribute no host-interface method,
package export, handle capability, or executable body. Every exported
history-only alias, shape, nominal, and constructor carries Go's standard
`// Deprecated: …` doc directive naming its signature removal version. The
directive propagates through the complete retained dependency closure. When a
declaration is reached from both live source and history, live provenance wins
and the shared declaration has no deprecation directive.

A removed `host type` that remains reachable from a frozen signature is a
deprecated concrete nominal (or, when parameterized, its deprecated
declaration-owned carrier). It is not a package-root generic parameter and
adds no role-adapter method. A host authored against current source therefore
selects and implements only live items.

### Host-type removal cannot preserve generic arity

Go has no optional or default type argument. Keeping a removed nullary host
type as a package-root parameter would force every new host to select a
history-only type, so the backend removes that parameter. An existing host
must drop the old argument from the host interface, handle, factory, and
any `Env_` / `Exp_` alias that carries the package-root parameters. Code that
directly used the removed selected type may also need to migrate to the
emitted deprecated nominal.
When emitted, the retained nominal remains source-addressable for inspection and migration,
but it is never a current host selection or package execution capability.
The degradation site is `GoHostBindingPlan::prepare` in
[`facade.rs`](../../kio-rs/src/backends/go/facade.rs), whose comment cites this
section.

### Incompatible retained declaration epochs are not source-stable on Go

One generated Go type identity cannot describe two incompatible exact
declaration epochs. Under the shared
[`README.md` rule](README.md#incompatible-retained-declaration-epochs), Go
omits every retained root, `Env_` / `Exp_` alias, nominal, and structural shell
that depends on the conflict. An old host's extra method remains structurally
acceptable only while every type in its signature is still nameable; affected
source must delete or rewrite that method signature and any direct reference
to an omitted generated type. Current hosts still implement and select only
live declarations.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs); its
comment cites the shared rule's explicit backend fan-out to this subsection.

Go remains **addition**-breaking, exactly as the cross-backend idiom notes: a new
`host fn` adds a method the host-contract interface now requires, so a host that
does not implement it fails to satisfy the interface — a Go **compile error** at
the host call site. The contravariant asymmetry — removals tolerated, additions
breaking — holds on Go as on every backend; only removal-side dependency
aliases remain, and the Rust backend's `#[deprecated]` method re-emit
([`rust.md` § Deprecated host items](rust.md#deprecated-host-items)) has no Go
analogue because Go needs none.

## Boundary semantics

Each cross-cutting property is inherited from [`README.md`](README.md);
the Go instantiations:

- **Synchronous calling convention** ([`README.md`](README.md)) — a host
  call and an exported invocation are each a direct Go method call. No
  async, no scheduler.
- **Type erasure at the FFI** ([`README.md`](README.md)) — the body is
  erased to `any`; the *skin* is fully typed. A type-variable position
  (a rank-N / existential binder, a higher-kinded carrier's payload)
  surfaces to the host as `any`, never a concrete type. Internal polymorphic
  callables retain the ordered stages specified by
  [`README.md` § 2](README.md#2-type-erasure-at-the-ffi): every binder of an
  escaped value is one hidden callable stage, while a direct known call may
  compact adjacent leading type applications. No such stage is a public Go
  parameter.
- **Facade topology and execution provenance** ([`README.md`
  § Facade topology and execution
  provenance](README.md#facade-topology-and-execution-provenance)) — one
  authoritative semantic topology determines Go's generic/row declarations,
  stable aliases, host interface, and export surface. Retained sites
  contribute only reachable declarations and aliases; only live sites can
  contribute methods, exports, handles, or body calls. Generic product
  declarations and the row/`Case()` sum representation are Go's
  host-language realization of that topology.
- **Open-world property** ([`README.md`](README.md)) — adding a
  declaration to a module body never changes the meaning of existing
  emitted code. Each site's authoritative semantic topology and
  live/retained provenance are fixed before rendering; Go emits only the shells
  and carriers reachable from that site. A shell name depends only on its
  structural kind and ordered semantic keys, a nominal-carrier name only on
  its exact qualified declaration identity, and concrete payloads appear only
  as generic arguments in stable site aliases. Neither unrelated declarations
  nor package-namespace occupancy participate, so adding one cannot rename an
  existing facade declaration or grant a retained site execution.
- **Package isolation**, **exception propagation**, **well-foundedness
  inheritance**, **behavioral additivity** ([`README.md`](README.md)) —
  inherited unchanged. A Kio `__absurd__` / bottom-typed dead end emits a
  Go `panic`; a host `exit(n)` propagates through the process exit code.

## FFI surface

### Atomic types

The host selects the public Go type for every nullary host declaration. For a
declaration carrying `role(r)`, the generated declaration-keyed adapter methods
convert that selected type to and from the following private body type:

| role | Go type | role | Go type |
| --- | --- | --- | --- |
| `i8` | `int8` | `u8` | `uint8` |
| `i16` | `int16` | `u16` | `uint16` |
| `i32` | `int32` | `u32` | `uint32` |
| `i64` | `int64` | `u64` | `uint64` |
| `i128` | `*math/big.Int` | `u128` | `*math/big.Int` |
| `f32` | `float32` | `f64` | `float64` |
| `bool` | `bool` | `str` | `string` |

`i128` / `u128` have no fixed-width Go primitive, so they map to
`*big.Int` from the standard library (`math/big`). This is **not** a
per-backend carve-out: `math/big` is standard and expresses
arbitrary-width integers exactly; the emitter injects the `math/big`
import when a file references `big.`. `()` (unit) is the canonical `Unit`
(`struct{}` — Go has no built-in unit value).

A parameterized host declaration `F[A, ...]` surfaces as the
declaration-owned `KioHostType_<exact-QTN>[A, ...]` carrier. Its type arguments
are the exact boundary types at that occurrence; it is never collapsed to
`any` or merged with another declaration. Go cannot abstract over a
host-selected type constructor, so the carrier stores the erased runtime value
privately and exposes the explicitly unsafe host integration pair
`UnsafeKioHostType_<exact-QTN>FromNative` / `UnsafeNative`. Those operations
wrap and unwrap only the private runtime carrier; the public application and
its declaration identity remain statically exact.

### Structural and nominal types

Go follows the structural FFI conventions
([`README.md` § Structural FFI shape conventions](README.md), including the
right-spine walk and 3-step key fallback). The authoritative semantic topology
is realized as follows:

- **Product** `(A & B & C)` → one flat generic `struct` shell with one
  type parameter and field per right-spine slot. The ordinary positional
  binary shell is `Product[T0, T1]`, with fields `F0` and `F1`; other key
  topologies use the reversible `KioFacade_V1_…` shell name. Field names
  follow the 3-step key fallback, rendered into Go's exact key codec when a
  bare identifier is not enough. Payload types do not participate in the
  shell name: a boundary site gives the concrete application a stable alias,
  for example `type Env_Api__pair_ret = Product[int32, string]`.
- **Sum** `(A | B | C)` → a concrete, row-anchored
  `KioSum[Row]`. The row records the ordered arm keys and payload types and
  self-anchors every constructor and case to that exact row, so cases for
  another sum topology cannot inhabit it. The site's stable declaration is
  `type <alias> = KioSum[<site-local Row>]`. For each arm `<i>`, `ffi.go`
  declares the typed case alias `<alias>_<i>` and the alias-local constructor
  `New<alias>_<i>(value) <alias>`. A host inspects a value through
  `value.Case()`, type-switches on those case aliases, and obtains the typed
  payload with `Value()`. The zero value of a concrete sum is valid:
  `Case()` reports its first arm with that arm's payload zero value.
- **Newtype** → when both members are public, carried with the established
  transparent representation: in the erased body it shares its payload's
  `[]any` rep, and the skin re-imposes no wrapper, so its boundary type is
  its instantiated payload's. With neither member or exactly one member
  public, the boundary type is instead that declaration's concrete,
  non-generic `KioNewtype_V1_…` carrier; the hidden payload does not
  contribute to an enclosing shape.
- **Function value** → a Go `func(...) ...` whose compound legs are
  converted across the boundary. The host sees that value-only callable;
  a boundary wrapper handles any internal polymorphic stages. A directly
  written `. -> R` is `func() R`. Instantiating `A -> R` with `A = .`
  preserves that existing argument slot and produces `func(Unit) R`.
- A free function-local **type-variable / abstract** position erases to
  `any`; exact host declarations and their applications do not.

A both-member **recursive newtype** whose boundary cycle is not cut by an
opaque or one-member carrier — one whose payload reaches itself, directly
(`List : . | (I32 & List)`) or through a chain of mutually recursive
newtypes (`Tree : I32 | Forest`, `Forest : . | (Tree & Forest)`) — has no
finite host shape, so it does not surface its payload type at the
boundary: it crosses as the erased `any` value the host round-trips through
the package's own constructors / projectors. Only a
non-recursive both-member newtype surfaces its instantiated payload type.
An opaque or one-member carrier is already a finite atomic leaf, so reaching
one stops this recursion test. This is exactly
as Rust crosses a recursive newtype as `Rc<dyn Any>`
([`rust.md` § Conversion semantics](rust.md)).

The `ffi.go` aliases name every compound, function, erased, Unit, or concrete
primitive value slot and return stably:
`Env_<member>_arg<i>` / `Env_<member>_ret` for host functions and
`Exp_<member>_arg<i>` / `Exp_<member>_ret` for exports and public-newtype
members. This includes primitive, `any`, `Unit`, product, sum, newtype, and
function types. A direct exact nullary host binding instead uses the package
root's type parameter: Go forbids a generic alias whose right-hand side is
only that parameter. `Forall` is transparent in this naming grammar and never adds
a `_result` component; a finite transparent both-member newtype likewise keeps
the incoming prefix while its payload is visited. Every product field
recursively contributes `<alias>_field<i>`, every sum-arm payload contributes
`<alias>_<i>_value`, and every visited cursor gets an alias. A function-valued
slot contributes `<alias>_cbarg<j>` and `<alias>_cbret`. When any of those
nested cursors is itself a sum, it gets the same full case-alias and
`New<alias>_<i>` constructor surface recursively. For example, a sum in product
field 1 uses `<alias>_field1_0` and `New<alias>_field1_0`, while a sum nested in
outer arm 0's payload uses `<alias>_0_value_0` and
`New<alias>_0_value_0`. A public newtype member's
alias treats the declaring module plus newtype as its owner and the member as
its leaf. An owner containing a source underscore uses the reserved exact
form, word-casing each component before encoding remaining `_` as `_u`
and `/` as `_s`; for example, newtype `A_b` member
`c` in module `api` contributes an
`Exp_KioItem_api__AB__c_<slot>` alias. This stays distinct from newtype `A`
member `b_c`, whose readable alias is `Exp_Api__A_bC_<slot>`.

### Carve-outs

None. Go expresses every spec- and IR-admitted FFI shape its host
language can carry: structural products as generic structs, sums as
row-anchored `KioSum` values with typed cases, all atomic roles (including
`i128` / `u128` via `math/big`), function values, and the erased
higher-kinded body.

## Item naming

At the FFI surface:

Go uses [shared source-component word casing](README.md#source-derived-public-names),
with title casing for exported leaves (`do_work` → `DoWork`).

- **The facade names** — the `package` clause is the namespace
  (§ Output layout); the handle is its public title brand (`greeter` →
  `Greeter`), the host contract the handle + `Host` (`GreeterHost`),
  the factory `Create` + the handle (`CreateGreeter`). One derivation
  root: every name is reconstructible from the `package` clause.
- **Host interface methods** — the readable export-capitalized
  `<module-with-_>__<leaf>` form when the module contains no source `_`;
  otherwise the reserved exact `KioItem_<encoded-module>__<leaf>` form
  (§ Host record contract).
- **Exact host bindings** — one `KioHost_<frame>` root parameter per nullary
  declaration, one `KioHostType_<frame>` carrier per parameterized
  declaration. `<frame>` is
  `V1_M<count>_C<len>_<segment>...N<len>_<leaf>`, so it encodes the complete
  qualified declaration identity without package occupancy or leaf lookup.
  Word-case every source component before escaping it and calculating its
  encoded byte length; fixed count, role, and index fields are unchanged.
- **Role adapters** — `KioHostIn_<identity>` / `KioHostOut_<identity>`, where
  ASCII-alphanumeric word-cased components are joined by `_`
  (`api.Count` → `api_Count`, `data_api.Native_count` → `dataApi_NativeCount`).
  A remaining affix underscore, another unspellable component, or the reserved leading `V1`
  class selects the exact `<frame>` instead. The choice depends only on that
  declaration, so adding another declaration cannot rename an adapter.
- **Exported items** — a root module and every function/member method use the
  natural/exact component rule: unmarked names use title word casing, while
  a leading or trailing underscore run selects `KioItem_<encoded-component>`
  (word casing, then `_` → `_u`). Nested module
  selectors always use `KioModule_<encoded-component>`, and public-newtype
  handles always use `KioType_<encoded-component>`. The role prefixes are
  disjoint, and every exact component is encoded independently.
- **Newtype carriers** — `KioNewtype_V1_M<count>_`, followed by one
  `C<len>_<component>` frame per module component and one
  `N<len>_<name>` frame for the newtype leaf. Each component is word-cased
  first. ASCII letters and digits remain
  literal, `_` becomes `_u`, and every other UTF-8 byte becomes `_x<hh>`;
  lengths count the escaped spelling. The complete qualified identity alone
  selects the concrete, non-generic carrier, so same-leaf declarations in
  different modules never share one.
- **Generic product and row declarations** — named from structural kind plus
  the ordered 3-step semantic keys, never from payload types. The common
  positional binary product is `Product`; every other topology uses the
  reversible `KioFacade_V1_<kind>_K<count>_…` spelling. The encoding is
  selected before package namespace validation and has no content hash,
  registry-occupancy suffix, or length-triggered alternate name. This opaque
  structural identity codec retains raw semantic keys; public word casing
  does not rewrite its identity bytes. Concrete
  payload types appear as generic arguments behind the stable boundary aliases.
- **`ffi.go` aliases** — `Env_<member>_<slot>` / `Exp_<member>_<slot>`.
  Host fns and ordinary exports use the same injective module/leaf name as
  the host interface. A public-newtype member additionally makes its type a
  distinct owner component; an underscore in that owner selects the exact
  `KioItem_<encoded-owner>__<leaf>` form described under § FFI surface. A
  sum alias derives only its indexed case aliases `<alias>_<i>` and
  constructors `New<alias>_<i>`; hosts need not name row helpers.

Product-field and sum-arm semantic keys follow the 3-step key fallback
([`README.md`](README.md)). A positional product field renders as `F<i>`
(Go-exported, so the field is host-visible); the stable sum API presents arms
by their alias-local index.
Named semantic field keys word-case their source components; positional
roles such as `F1` and `K1` are not source words and retain their fixed spelling.

## Worked example

A package `greet` with a host `print` and an exported `main`:

```kio
// greet.pkg.kio
package greet;
build { cache "out/.kio-cache/"; target go { out "out/go/"; } }
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

`kio build go` writes `out/go/{pkg.go, host.go, shapes.go, ffi.go,
kio_runtime.go}`, each declaring `package greet`; the handle is `Greet`,
the host contract `GreetHost`, the factory `CreateGreet`. A Go host —
with its `go.mod` mapping the import path onto the emitted directory
(`require greet v0.0.0` + `replace greet => ./out/go`):

```go
package main

import "greet"

type Stdout struct{}

func (Stdout) App__print(arg0 string) { print(arg0) }

func main() {
    pkg := greet.CreateGreet(Stdout{})
    pkg.App.KioModule_main.Main()
}
```

This prints `hello` and exits 0.

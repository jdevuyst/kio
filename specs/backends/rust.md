# Rust backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `erased-static` (see [`README.md` § Language families](README.md#language-families)).

> **Known host-source compatibility caveat.** Removing a bridged `host type`
> requires an existing host to delete the stale associated-type entry from its
> host-trait `impl`. See
> [§ Host-type removal is not source-stable on Rust](#host-type-removal-is-not-source-stable-on-rust).
> When retained signatures require incompatible epochs of one exact nominal
> declaration, Rust also omits the affected trait methods and type closure;
> existing host source must delete or rewrite those trait items and generated
> type references. See
> [§ Incompatible retained declaration epochs are not source-stable on Rust](#incompatible-retained-declaration-epochs-are-not-source-stable-on-rust).

The `"rust"` build target ([`specs/package.md` § Build target
files](../package.md#build-target-files)) emits a Rust crate. This
page describes the surface a Rust host calls against.

## Language version

Emitted code targets **Rust 2024 edition** (Rust 1.85+). Kio's bottom type uses
the stable `std::convert::Infallible` facade.

All other constructs use stable Rust: tuple structs, generic type parameters,
associated types, sealed trait emulation via a private supertrait, and derived
or explicit `Clone` / `PartialEq` implementations. The facade does not depend
on generic associated-type equality for higher-kinded application.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target rust {
    out "out/rust/";
    namespace "my_kiopkg";
  }
}
```

`kio build rust` writes a self-contained Cargo crate under the
`out` directory:

- `Cargo.toml` — crate manifest; `name = "my_kiopkg"` (from the
  `namespace` build-block key, defaulting to the kio package name
  if absent; a default that collides with a Rust keyword uses
  `__kio_pkg_<escaped-source>`, with `_` escaped as `_u`),
  `edition = "2024"`, zero external dependencies.
- `src/lib.rs` — the package's invocable surface. A single
  `pub fn create_myKiopkg<H: MyKiopkgHost>(host: H) -> MyKiopkg<H>`
  factory returns the root handle. That handle exposes one field per bridged
  top-level module, and each `export` entry is a method on its generated module
  namespace handle.
- `src/shapes.rs` — the public representation-witness algebra and the exact
  facade values it names: sealed `KioType` markers, `KioNative<T>`, opaque
  `KioStoredValue` / typed `KioValue<A>`, canonical `KioProduct` / `KioSum`
  markers and `Product` / `Sum` facade values, `KioFunctionN` / `KioFnN`, and
  the `KioAppliedN` / `KioApplyN` application forms used by abstract type
  constructors. Products and sums use the canonical right-nested facade
  directly; Rust emits no second semantic-keyed structural wrapper. Nominal
  declarations live under
  `shapes::nominal::<declaring module path>::<TypeName>`. Both mappings are
  deterministic and independent.
- `src/host.rs` — the `pub trait MyKiopkgHost` interface (one required method
  per live bridged `host fn`, plus any optional deprecated retained defaults;
  each name carries the declaring module's namespace —
  see [§ Host record contract](#host-record-contract)).
- `src/ffi.rs` — a generated convenience module naming each non-Unit,
  nameable Rust type at a host-`fn` / `export`-`fn` boundary slot via a type alias
  (reached as `crate::ffi::env::<member>::<slot>` /
  `crate::ffi::exp::<member>::<slot>`). The alias expands to the canonical
  facade and introduces no new boundary type or host-trait constraint. Unit
  carries no shape and therefore has no alias. The complete slot grammar is
  described in [§ Higher-rank value parameters](#higher-rank-value-parameters). Like
  `src/__kio_runtime.rs`, this file is
  overwritten on every build; host code may use these aliases when a stable
  per-member spelling is more convenient than the underlying facade path.
- `src/__kio_runtime.rs` — the internal implementation behind the opaque
  `KioStoredValue` token. Its raw default `Rc<dyn Any>` operations, or
  `Arc<dyn Any + Send + Sync>` operations under the thread-safe build option,
  remain crate-private; public code crosses through a generated `KioType`
  conversion, `KioValue<A>`, a declaration-owned constructor witness, or a
  generated host carrier. The file is **not
  user-modifiable** — the emitter overwrites it on every build,
  and its content is the same byte-for-byte across packages built in the same
  thread-safety mode. Host code should not import from it.

The rust backend accepts a `namespace` build-block key setting the
crate name — see
[`specs/package.md` § Per-target keys](../package.md#per-target-keys).
An explicit value must match `[A-Za-z_][A-Za-z0-9_-]*` and must not
collide with a Rust keyword after Cargo's `-` → `_` mapping; a value
outside that grammar is a build error.

The facade's branded public names derive from the effective namespace after
Cargo's `-` → `_` crate-identifier mapping. An ordinary source-shaped name
uses title word casing for the handle (`my_kiopkg` → `MyKiopkg`); the host
contract is the handle + `Host` (`MyKiopkgHost`). The factory keeps the
fixed `create_` prefix followed by the value brand (`create_myKiopkg`).
Affixed source-shaped names and reserved defaults use `KioPkg_...` brands;
other explicit namespaces use `KioNs_<hex-UTF-8>` as in
[the shared brand encoding](README.md#branded-naming). Those framed brands keep their
capitalization in factories (`create_KioPkg_Match` for default package
`match`, whose crate is `__kio_pkg_match`). One derivation root means a host — which authored
the manifest, so it knows the crate name without reading build output —
reconstructs the whole facade from the crate's published name alone. The
internal `host` / `shapes` / `ffi` / `__kio_runtime` modules keep their
fixed names (the trait lives at `crate::host::MyKiopkgHost`). Generic facade
support names derive from their exact declaration or site identities, and
export-namespace struct names derive from exact module paths; both remain
scoped by the branded crate. Because no emitted type or factory carries a fixed
package-independent name, two crates with distinct namespaces coexist in one
host build without collision.

The emitted crate compiles with `cargo build` from inside the
output directory; it has no external `[dependencies]` beyond the
standard library. The runtime-support file `src/__kio_runtime.rs`
is internal to the emitted crate — it is not a separate dependency
the host installs.

### Thread safety

By default the emitted crate uses a **single-threaded** value
representation: `KioStoredValue` owns an `Rc`-backed erased token, callable and
existential carriers use that token, the host trait is bounded `Clone +
'static`, host storage types are `Clone + PartialEq + 'static` plus any
role-specific `From<primitive>` bound, and every generic Kio type parameter is
a `KioType` marker whose `Facade` is `Clone + 'static`. `Rc` is not
thread-safe, so an
emitted package value cannot cross a thread boundary. This is the
behavior when the `thread_safety` build-block key is absent.

The `thread_safety` key (see
[`specs/package.md` § Per-target keys](../package.md#per-target-keys))
opts the crate into a thread-safe representation. Under any opt-in
value the emitter swaps the wrap to `Arc<dyn …>` and adds `Send +
Sync` marker bounds throughout:

- `KioStoredValue` uses `Arc<dyn Any + Send + Sync>` rather than
  `Rc<dyn Any>`, and `KioFnN::new` requires a `Send + Sync` closure;
- the host trait header becomes `MyKiopkgHost: Clone + Send + Sync +
  'static`, associated types `Clone + PartialEq + Send + Sync +
  'static` plus any unchanged `From<primitive>` bound, and every `KioType`
  facade satisfies `Clone + Send + Sync + 'static`;
- marker-directed storage conversions and generated host carriers retain the
  same public shapes with the matching thread-safe bounds.

**The host's obligation under an opt-in:** every host-side host-trait
implementation must satisfy the marker bounds — its associated
types and any value it supplies at a capture position must be `Send +
Sync`. A host impl that doesn't is rejected with a standard Rust
`Send`/`Sync` diagnostic at host-impl time; the `thread_safety`
build-block key is the cause.

**All three opt-in values select the same representation.** The
build-block key admits `send`, `sync`, and `send_sync`; the Rust backend
realizes each as the full `Arc<dyn … + Send + Sync>` shape above. That
representation satisfies every opt-in: package values may be moved between
threads, shared between threads, or both. Consequently, every accepted
opt-in carries the same host obligations: host implementations, associated
types, capture values, and erased type parameters must satisfy both `Send`
and `Sync`. The shared marker set is applied in
`kio-rs/src/backends/rust/thread_safety.rs` (`ThreadSafety::marker_suffix`).

## Loading the package

A host that wants to call into the package performs three steps
in order.

### 1. Add the crate as a dependency

Either via `path` for a local development build:

```toml
[dependencies]
my_kiopkg = { path = "./out/rust" }
```

or by publishing the emitted crate to a private registry, then
adding it by version like any ordinary crate.

### 2. Implement the host interface

`my_kiopkg::host::MyKiopkgHost` is a trait listing one associated entry per
live bridged `host type` and one required method per live bridged `host fn`,
plus any optional deprecated retained defaults described below. A nullary
declaration's entry is its exact selected facade type. A parameterized
declaration's entry is its declaration-owned, arity-independent `Storage`
type; its type arguments remain on the generated marker and facade carrier
rather than on a GAT.
Each method name carries the declaring module's namespace. A module path with
no `_` keeps the established `/` → `_` spelling. A path containing `_` uses a
reserved `__kio_host_` prefix, word-cases each source component, then encodes
remaining `_` as `_u` and `/` as `_s`; `__` separates the module from the
word-cased item leaf (see
[§ Host record contract](#host-record-contract)). Thus a
`host fn print` in module `app` becomes `app__print`. The host implements
the trait with whatever backing logic it likes:

```rust
use my_kiopkg::host::MyKiopkgHost;

#[derive(Clone)]
struct MyHost;

impl MyKiopkgHost for MyHost {
    type app__String = String;

    fn app__print(&self, s: Self::app__String) {
        print!("{s}");
    }
}
```

### 3. Instantiate and invoke

```rust
let pkg = my_kiopkg::create_myKiopkg(MyHost);
pkg.app.main.main();
```

`create_myKiopkg` returns a `MyKiopkg<MyHost>` whose surface mirrors the
bridged exports under their module namespace: an export `main` from module
`app/main` is reached as `pkg.app.main.main()`. The host is moved into
the package; the package owns the host for its lifetime.

## Host record contract

The host record is the `pub trait MyKiopkgHost` in `src/host.rs`, rendered
from the backend-agnostic host-trait descriptor (see
[`README.md` § 8 Host-trait descriptor](README.md#8-host-trait-descriptor)).
The trait declares:

1. One module-qualified associated entry per `host type`, across every bridged
   module. A nullary declaration selects its exact Rust facade type. A
   parameterized declaration selects one invariant `Storage` type used by the
   declaration-owned carrier at every application.
2. One required method per live `host fn`, across every bridged module, plus
   any optional deprecated retained defaults described below. A live method's
   signature mirrors the Kio `host fn` declaration, with Kio types mapped to
   Rust per the FFI surface below.

**Namespaced method names (rung 2).** The host boundary is
module-qualified, so each method name carries its declaring module's
namespace. A module path containing no `_` replaces `/` with `_`, appends
`__`, then appends the word-cased item leaf. A path containing `_` instead
uses the reserved prefix `__kio_host_`, word-casing each component before
encoding remaining `_` as `_u` and `/` as `_s`
before the same `__` separator. A `host fn print` in module `app`
surfaces as `fn app__print(&self, …)`; the same leaf in module `util/io`
surfaces as `fn util_io__print(&self, …)`. The reserved prefix is outside the
image of ordinary module paths. For example, `data_api.do_work` becomes
`__kio_host_dataApi__doWork`, while `data/api.do_work` becomes
`data_api__doWork`. The Rust
backend renders the namespaced boundary as one flat host trait
(`MyKiopkgHost`) with mangled-but-injective method names (rung 2 of the
namespaced-boundary scheme) rather than nested sub-traits (rung 1); this
is a rendering choice — nullary host types and parameterized declarations'
`Storage` entries live on that one trait, while declaration-owned marker/facade
carriers name applications (see [§ Non-atomic host types](#non-atomic-host-types)).
Splitting into nested sub-traits would force generic sub-trait bounds at every
cross-module host-type reference. Identity is preserved either way, so it owes
no per-backend carve-out (see [`README.md`](README.md)).

Every nullary `host type` surfaces as its own associated type. Its name includes
the declaring module, so `host type Text role(str);` in `app` becomes
`type app__Text: Clone + PartialEq + 'static + From<String>;`. A role is a
capability, not a representation choice: `role(i32)` requires the selected
type to implement `From<i32>`, `role(bool)` requires `From<bool>`, and
`role(str)` requires `From<String>`. Role-bearing host types are nullary. The
backend emits neither custom conversion methods nor `AsRef` bounds, and it
does not specialize a role type to the corresponding Rust primitive.

A `role(str)` type has one canonical facade: the selected associated type by
value. Direct parameters, returns, structural slots, stored values, generic
substitutions, and callable legs all use `Self::app__String`, never
`&Self::app__String`. This is required by the `KioType::Facade` equation: one
Kio type cannot change Rust representation according to occurrence.

The host may borrow that owned value inside its method implementation or write
its own local adapter around the generated method. No borrowing wrapper is
part of the generated Kio ABI. Because the canonical ABI is already owned, a
source `{ owned }` annotation does not select another Rust facade.

The trait's current obligations are exactly the live declarations selected by
the package bridge — one exact facade type per nullary `host type`, one exact
`Storage` type per parameterized host declaration, no per-role conversion
methods, and no ambient runtime. Representable retained methods may also appear
as optional deprecated defaults under the next section; they never become a
current host obligation.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) for
Rust, and the Rust backend's source-stability limitation that idiom permits.

Removing a `host fn` from a bridged module is a compatible env-side change at
the language level (see
[`../versioning.md` § The compatibility
relation](../versioning.md#the-compatibility-relation)): the package no longer
requires it, so the package still loads against an existing host. But the
regenerated host trait no longer declares the method, while the host's
hand-written `impl MyKiopkgHost` still defines it — an orphan method, a Rust
compile error (E0407). To keep the host's source compiling across the removal,
the Rust backend **re-emits the removed `host fn` as a `#[deprecated]` trait
method with a diverging default body**, its signature recovered from the
changelog history (the item's introducing `add` / `modify` block — see
[`../versioning.md` § Deprecated host
items](../versioning.md#deprecated-host-items)):

```rust
pub trait MyKiopkgHost: Clone + 'static {
    // … live methods …

    #[deprecated(note = "host fn `app__log` removed at v(3)")]
    fn app__log(&self, s: Self::app__String) -> Self::app__I32 {
        unimplemented!("host fn `app__log` removed at v(3)")
    }
}
```

The default body **diverges** (`unimplemented!`) rather than returning a value:
a non-`()` return type has no value the package can synthesize — only the host
can produce, say, a `Widget` or a sum — so a synthesized non-diverging default
is not generally possible. The diverging default is used **uniformly**, even
for a `()`-returning fn that could take an empty body, so the deprecation trap
is consistent regardless of return type. This is safe: the package no longer
*calls* the removed fn, so the default body is unreachable from package code. A
new host that never overrode the method inherits the trap but never hits it; an
existing host's `impl` still provides the method and overrides the default
unchanged. The panic is a contract-panic in generated glue — the host's
obligation — not a runtime hazard, and is the deliberate form: a
believed-unreachable internal contract violation panics rather than
returning a recoverable error.

The deprecated trait method remains callable by host source, but it is not a
live package capability. Under [`README.md` § Facade topology and execution
provenance](README.md#facade-topology-and-execution-provenance), its frozen
signature may retain trait and shape declarations while no live host binding
or package call path refers to the method. A public nominal newtype mentioned
only by that frozen signature likewise keeps its exact declaration-owned
carrier and any type-constructor marker needed to name an application in the
signature. Retained parameterized-host applications keep their generated
marker/carrier closure only while the declaration's storage entry remains
live and nameable. Retention alone never restores a newtype's constructor or
projector methods or imposes a removed storage choice on a current host.

Rust can retain the optional method only while its complete frozen signature
is nameable without a removed host associated type. If it depends on a removed
`host type`, the backend omits that deprecated method too: restoring the
associated type would make history mandatory for every new host, while a free
alias cannot stand in for `Self::Assoc`. An old host then deletes both the
stale associated-type definition and any stale method override. History-only
support declarations that remain nameable are `#[deprecated]`; a support
identity also reached by live code stays live and nondeprecated.

The deprecation window — how many sealed generations the Rust backend keeps
re-emitting a removed `host fn` — is emitter policy read from the changelog
history, per the cross-backend idiom.

### Host-type removal is not source-stable on Rust

Removing a `host type` is **a genuine Rust source-stability limitation, not a
zero-touch change.** Every host type contributes a host-trait associated
entry: its exact facade type when nullary, or its invariant `Storage` type when
parameterized (see [§ FFI surface](#ffi-surface)).

A nullary `host type Foo;` emits as `type <module>__Foo: …;`; a parameterized
`host type D[A];` emits `type <module>__DStorage: …;`. When either declaration
is removed, no retained method is emitted if its frozen signature still needs
that associated entry; this spares a *new* host from declaring history. But the
*existing* host's `impl MyKiopkgHost` still contains
`type <module>__Foo = …;` for an associated type the trait no longer declares —
which is itself a Rust compile error (E0437, the orphan-associated-type
direction). The host must delete that `type … = …;` line by hand. Rust has no
stable `associated_type_defaults` that would let the trait retain a defaulted
associated type and keep the host's declaration valid, so there is no
source-stable re-emit for this case.

This is a genuine limitation of the Rust language, not deferred work: no escape
hatch (a defaulted associated type, a hidden shim) keeps a removed associated
type valid in an unchanged host `impl`. **Host-type removal therefore requires a
host source edit on Rust** (delete the stale `type … = …;`, plus a stale method
override whose signature depends on it). A host migrating across an independent
`host fn` removal does not edit its impl. The rule is uniform for role-bearing
nullary, roleless nullary, and parameterized host types.

The exact emitter site is the retained-site filter in `emit_deprecated_host_items` in
[`kio-rs/src/backends/rust/emit.rs`](../../kio-rs/src/backends/rust/emit.rs),
whose comment cites this subsection in turn.

### Incompatible retained declaration epochs are not source-stable on Rust

One generated Rust type identity cannot describe two incompatible exact
declaration epochs. Under the shared
[`README.md` rule](README.md#incompatible-retained-declaration-epochs), Rust
omits every retained trait method, carrier, and support declaration that
depends on the conflict. An affected old `impl` must delete or rewrite the
stale method item and any reference to an omitted generated type; otherwise it
has an orphan trait method or an unresolved type. Current implementations
still supply only live items.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs); its
comment cites the shared rule's explicit backend fan-out to this subsection.

### Function arity follows the Kio type

Rust adds no backend-specific arity relation. `(A & B) -> C` and `A -> B -> C`
are different Kio/System-F types: the first consumes one product-domain layer,
while the second consumes two application layers. Changing between them is an
ordinary type modification under the
[`kio sig` compatibility relation](../versioning.md#the-compatibility-relation),
not a type-preserving arity reshape.

## Boundary semantics

This backend inherits the cross-cutting properties from
[`README.md`](README.md). The notes below cover only the
Rust-specific instantiations.

- **[§ 1 Synchronous calling.](README.md#1-synchronous-single-threaded-re-entrant-calling-convention)**
  A host calling `pkg.x()` blocks until `x` returns; package code
  invoking a host-trait method blocks until the host's
  implementation returns. The package runs on whichever thread
  invoked it and inherits the host's threading model. Re-entrancy
  is permitted: a host method invoked from a package call may call
  back into `MyKiopkg<H>` methods.
- **[§ 2 Type erasure.](README.md#2-type-erasure-at-the-ffi)**
  Polymorphism maps to representation-marker generics. A `[A]`-quantified
  `host fn` `host fn id[A](v: A) -> A;` in module `m` emits
  `fn m__id<A: KioType>(&self, v: A::Facade) -> A::Facade;`. A unique Kio
  type-parameter name stays readable. When distinct source binders with the
  same spelling enter one Rust generic scope, later binders receive a
  deterministic backend-reserved spelling keyed by their semantic binder
  identity; their source order and references are unchanged. See
  [§ Item naming](#item-naming).
  A rank-N value parameter uses a site-owned public callable whose generic
  method retains the inner marker binder; only the package body's backing
  value is erased. Ordinary function values use the named
  `KioFunctionN<...>` marker and `KioFnN<...>` facade at every occurrence.
- **[§ 3 FFI additivity.](README.md#3-ffi-additivity)**
  Adding `pub` items to a bridged module's source tree is strictly
  additive at the emitted crate: new methods appear on that module's generated
  namespace handle alongside existing ones without breaking host call sites.
  `host fn` additions are *not* covered — they extend the host trait, which is
  a contract-surface change.
- **[§ 5 Exception propagation.](README.md#5-exception-propagation)**
  A host method that panics propagates through the Kio call stack
  unchanged and emerges from the originating `pkg.x()` call site.
  The package wraps no host invocation in `catch_unwind`. The
  package's own runtime guards (most commonly `__absurd__` on an
  exhausted `!` value) panic through the host call stack the same
  way.
- **§§ 4, 6, 7** (package isolation, well-foundedness inheritance,
  behavioral additivity) apply unchanged.
- **Open-world compilation.** Each generated marker, nominal carrier,
  constructor witness, rank-N site, parameterized-host carrier, and optional
  presentation helper is keyed only by its exact prepared type/declaration/site
  identity. `KioType` implementations are local and sealed; trait-implementation
  discovery, module occupancy, and unrelated declarations do not select an
  existing facade equation. Adding a declaration therefore adds only newly
  reachable items and cannot change the marker or `Facade` of an existing
  boundary occurrence.

## FFI surface

### Atomic types (governed by `role`)

A `host type Foo role(r)` declaration remains an exact host-selected Rust
type. The role adds the standard construction capability for its literal
primitive:

| `role(...)` | Required associated-type bound |
| --- | --- |
| `role(i8)` … `role(i128)` | `From<i8>` … `From<i128>` |
| `role(u8)` … `role(u128)` | `From<u8>` … `From<u128>` |
| `role(f32)`, `role(f64)` | `From<f32>`, `From<f64>` |
| `role(bool)` | `From<bool>` |
| `role(str)` | `From<String>` |

These bounds apply to the declaration, whether or not a particular host fn
currently consumes a literal. They are the only role-specific conversion
surface: there are no generated converter methods, `AsRef` requirements, or
primitive passthrough special cases. A host may select the primitive itself or
a local nominal wrapper that implements the corresponding standard `From`
trait. All host associated types also carry the ordinary `Clone + PartialEq +
'static` bounds.

The typechecker admits a lexical literal only when its primitive kind matches
the `role` on the exact host-type declaration selected by annotation or
context. Selecting an exact type never bypasses that match: a mismatched role,
or a roleless host type, is rejected before emission. An admitted literal is
constructed as the selected associated type through that declaration's
`From` implementation; Rust exposes no all-shape literal ingress. A
conditional over a `role(bool)` type compares its exact value with the exact
value constructed from `true`; it does not compare the host value directly
with primitive `bool`.

`role(str)` uses the selected associated type by value in every public
occurrence. A direct parameter in module `app` is
`Self::app__String`; a structural slot contains that same value; a callable
leg and `ffi.rs` alias name it without an added lifetime. This uniform owned
facade is what lets `KioNative`, structural markers, and function markers
compose without occurrence-sensitive exceptions.

### Non-atomic host types

A nullary `host type Foo;` declaration without a `role(...)` annotation uses
the same exact associated-type representation as a role-bearing host type, but
adds no `From<primitive>` capability. A parameterized declaration instead has
one declaration-owned storage entry and generated marker/facade types. Its
storage is deliberately independent of the type arguments: the exact Kio
application is carried by the local marker, and only that generated carrier
can translate the storage token at the public boundary.

```kio
// module app
host type Array[T];
host fn array_push[T](a: Array(T), v: T) -> .;
```

emits

```rust
pub trait MyKiopkgHost: Clone + 'static {
    type app__ArrayStorage: Clone + PartialEq + 'static;

    fn app__array_push<A: KioType>(
        &self,
        a: KioHostType_app__Array<Self, A>,
        v: A::Facade,
    );
}

pub struct KioHostTypeMarker_app__Array<H: MyKiopkgHost, A: KioType>(/* private */);
pub struct KioHostType_app__Array<H: MyKiopkgHost, A: KioType>(/* private */);

impl<H: MyKiopkgHost, A: KioType> KioHostType_app__Array<H, A> {
    pub fn from_storage(value: H::app__ArrayStorage) -> Self;
    pub fn storage(&self) -> H::app__ArrayStorage;
    pub fn into_storage(self) -> H::app__ArrayStorage;
}
```

The generated marker implements `KioType` with
`Facade = KioHostType_app__Array<H, A>`. The facade privately carries a
`KioStoredValue`; its three storage methods are the only host integration
surface. `storage()` recovers a clone of the selected storage.
`into_storage()` additionally consumes the carrier, but it also recovers a
clone because the opaque token may be shared. Both operations perform a
constant-time token downcast followed by the host-selected `DStorage::clone`
cost. The carrier's `Clone` clones the token and its `PartialEq` decodes and
compares the selected `DStorage`. The constructor marker for the declaration
implements `KioTypeConstructor1` with `Apply<A> =
KioHostTypeMarker_app__Array<H, A>`. A direct application therefore selects
that declaration-owned marker and facade. An abstract occurrence instead uses
`KioApplied1` / `KioApply1`; the constructor marker's `lift` and `project`
methods relate that opaque abstract carrier to the exact direct facade.

The host owns the invariant that every token it places under an applied
carrier was encoded for that exact declaration-and-application marker.
Attaching it to a marker whose storage contract it does not satisfy violates
the invariant. If the
selected decoder is representation-incompatible, decoding deterministically
panics. A representation-compatible decoder may instead succeed, but that
possibility is not a compatibility guarantee for any marker pair and does not
make the attachment valid. Generated conversions are safe Rust and contain no
unchecked cast, so neither outcome causes undefined behavior. The storage may
use reference identity for `PartialEq`; the trait does not require structural
equality of hidden native contents. This declaration-owned carrier is required
for every parameterized host type, including familiar `Box`- or
`Array`-shaped declarations. Rust's stable type system cannot state an
arbitrary host-selected type-constructor equality, so the public contract does
not claim one.

An external mutable array binding can use the public opaque-token protocol
without exposing or guessing `A::Facade` inside its invariant storage:

```rust
#[derive(Clone)]
struct ArrayStorage(Rc<RefCell<Vec<KioStoredValue>>>);

impl PartialEq for ArrayStorage {
    fn eq(&self, other: &Self) -> bool { Rc::ptr_eq(&self.0, &other.0) }
}

impl MyKiopkgHost for MyHost {
    type app__ArrayStorage = ArrayStorage;

    fn app__array_push<A: KioType>(
        &self,
        a: KioHostType_app__Array<Self, A>,
        value: A::Facade,
    ) {
        let storage = a.storage();
        storage.0.borrow_mut().push(KioValue::<A>::pack(value).into_stored());
    }
}
```

A matching read removes or clones the stored token, calls
`KioValue::<A>::from_stored(token).unpack()`, and returns `A::Facade`. Using a
different marker at that point always violates the host invariant. An
incompatible representation deterministically panics; a compatible
representation may decode, without guaranteeing that outcome for any marker
pair.

### Structural and nominal types

Anonymous structural values built from `&` / `|` cross the FFI as the
Rust-native `Product` record and `Sum` enum. A `labels` declaration instead
creates a declaration-owned nominal carrier with private erased storage; the
host constructs or inspects that value only through the declaration's
generated public constructor/projector capabilities. Labels never turn that
nominal carrier into an anonymous structural record or enum.

Rust realizes the shared [facade-topology and execution-provenance
contract](README.md#facade-topology-and-execution-provenance) through a
compositional marker translation. Every saturated Kio type has a local sealed
marker implementing `KioType`; its associated `Facade` is the Rust value type
that crosses the boundary. The canonical structural equations are:

```rust
KioProduct<A, B>::Facade = Product<A::Facade, B::Facade>
KioSum<A, B>::Facade     = Sum<A::Facade, B::Facade>
```

Because Kio's `&` / `|` trees are binary and right-associated, these equations
apply recursively. `A & B & C` therefore has facade
`Product<A::Facade, Product<B::Facade, C::Facade>>`; it is not a distinct
three-payload signature wrapper. Substituting `T := KioProduct<B, C>` into
`KioProduct<A, T>` produces that same Rust type definitionally. Sums obey the
same equation. This substitution-stable equation is part of Rust's
compositional marker facade; it does not prescribe another backend's public
structural representation.

`Product<A, B>` is a public struct with fields `_0` and `_1`; `Sum<A, B>` is a
public enum with variants `Left` and `Right`. Both implement `Clone` only when every
payload does, and `PartialEq` only when every payload does. Rust uses these
canonical binary types directly. Stable per-boundary aliases in `ffi` may name
an occurrence, but no semantic-keyed structural wrapper competes with the
signature type. Adding another declaration therefore cannot change an
existing facade or alias.

The marker/facade leaves map as follows:

- Canonical Unit and Bottom have generated local markers whose facades are
  `()` and `Infallible` respectively.
- An exact nullary host atom has a generated marker whose facade is the host's
  module-qualified associated type. A parameterized host application uses its
  generated marker and `KioHostType_<declaration><H, ...>` facade.
- A quantified kind-`*` type is a surrounding `A: KioType` marker; the value
  occurrence is `A::Facade`. At a host-authored concrete call,
  `KioNative<T>` supplies `Facade = T` for any `T: Clone + 'static` without
  requiring the host to implement the sealed trait.
- A function uses `KioFunctionN<...>` as its marker and `KioFnN<...>` as its
  facade at every occurrence.
- An abstract constructor application uses `KioAppliedN<F, ...>` as its marker
  and `KioApplyN<F, ...>` as its facade.
- A public newtype has a declaration-owned marker whose facade is
  `shapes::nominal::<declaring module path>::<Name><H, ...>`; its selected
  constructor and projector are its only payload capabilities.

Examples (where `T1` and `T2` are public newtypes):

| Kio type | Rust boundary type |
| --- | --- |
| `I32 & Str` | `Product<H::app__I32, H::app__Str>` |
| `I32 \| Str` | `Sum<H::app__I32, H::app__Str>` |
| `T1 & T2` | `Product<T1<H>, T2<H>>` |
| `T1 \| T2` | `Sum<T1<H>, T2<H>>` |
| `I32 & Str & Bool` | `Product<H::app__I32, Product<H::app__Str, H::app__Bool>>` |

#### Conversion semantics

The package's **internal body rep is erased** as crate-private raw
`Rc<dyn Any>` values, or `Arc<dyn Any + Send + Sync>` under the thread-safe
build option. Boundary marker codecs wrap that raw representation in the
opaque, cloneable `KioStoredValue` carrier whose raw operations remain
crate-private (§ Higher-kinded types). A product is a nested-binary pair erased
at each level; a 3-product `(A & B & C)` stores `(a, (b, c))` through those raw
values. A sum is an erased tagged payload. The
nested-binary spine matches the cross-backend right-spine walk, so the
body's `fst` / `snd` / tag-test operations run uniformly over the erased
rep with no per-shape machinery. `KioProduct` / `KioSum` reconstitute the same
binary facade spine; boundary aliases never enter the body.

The per-signature wrapper at every exported-function / host-fn boundary
bridges the erased internal rep and the exact typed facade on the way
out and back on the way in:

- **Export entry.** Each parameter converts from its marker's `Facade` through
  `KioType::into_stored` before the body sees it. Structural marker conversions
  recurse over the exact binary product/sum spine.
- **Export return.** The body's stored result converts through
  `KioType::from_stored` to the exact facade before crossing back to the host.
- **Host call.** Each value-arg whose declared slot type is a compound
  converts from erased to its exact typed facade before the host receives
  it; the host's returned compound converts from the typed facade
  back to stored form on the way into the body. Generic-position slots
  (`state: S` in `fn loop[S][R](p0: …, state: S) -> R`) cross as
  `S::Facade` for `S: KioType`; containers retain them as `KioValue<S>` and
  the body holds their opaque stored token.
- **Callable slots.** A function crossing the FFI uses the named
  `KioFnN<...>` facade. `KioFnN::new` accepts a Rust closure and `call` invokes
  it through the marker-directed conversions. Its declared parameter and
  return types live on the FFI surface independently. The callable's arity follows
  the cross-backend value-group rule: a `fn step(callback: ((I32 & Str & Bool)
  -> I32)) -> I32` receives a callback with one product-valued argument,
  callable with the canonical right-nested product facade (and its stable
  alias in `ffi.rs`). The product
  slot stores each associated type by value, including its `Str` field. The
  package-side wrapper rebuilds
  the erased nested-binary value when the Kio body binds the whole
  product, and it performs the symmetric conversion for compound return
  legs and for closures the host hands back to the package.
  Generic-position legs (a tparam-typed param or return) flow through the
  marker's `Facade` at the FFI and its stored token inside the body — the same
  rule as the host-call generic slot above.

**Recursive newtypes.** Every public newtype, including a directly or mutually
recursive one, crosses as its exact declaration-owned nominal carrier. The
carrier keeps erased recursive storage private and exposes the payload only
through the selected constructor/projector methods, whose signatures retain
the exact prepared payload facade. A recursive occurrence therefore remains
the same nominal Rust type in an enclosing product, sum, constructor, or
projector signature; `KioStoredValue` never becomes a nominal value's public
facade type.

Hosts must not introspect a value of structural type across Kio versions
assuming a particular *internal* shape — the body rep is erased and internal;
the canonical `KioType::Facade` equation is the public contract.

**Equality.** `Product` / `Sum` implement `PartialEq` exactly when all of their
concrete payload arguments do. A nominal carrier whose payload is
private erased storage exposes no `PartialEq` implementation merely by virtue
of that storage; no unconditional derive strengthens the declaration's
capabilities.

**Mutation and aliasing.** Facade conversion owns or clones values according to
the public method signature. `KioStoredValue::clone` clones its reference-counted
token. A declaration-owned parameterized-host `storage()` clones `DStorage`;
`into_storage()` consumes the carrier but also recovers a clone because the
token may be shared. The package does not obtain a mutable native reference
through the opaque token.

### Carve-outs

There is no occurrence-sensitive `impl Fn` / `Rc<dyn Fn>` split in the public
type translation. Every ordinary function type uses the same named marker and
facade, including parameters, returns, structural slots, newtype payloads, and
generic substitutions:

```rust
pub struct KioFunction1<A: KioType, R: KioType>(/* marker */);
pub struct KioFn1<A: KioType, R: KioType>(/* private stored token */);

impl<A: KioType, R: KioType> KioFn1<A, R> {
    pub fn new(f: impl Fn(A::Facade) -> R::Facade + 'static) -> Self;
    pub fn call(&self, a: A::Facade) -> R::Facade;
}
```

An ordinary function keeps its declared ABI arity, so a product domain with
arity one remains one product-valued Rust argument. An outer-`Forall`
dictionary payload retains the staged structure described by
[`README.md` § Function-type FFI canonicalization](README.md#function-type-ffi-canonicalization),
but its final value-function stage is still a `KioFnN` facade.

`N` comes from the prepared callable's source value-stage grouping, never from
recursively flattening a parameter's type. Thus `A -> R` is
`KioFunction1<A, R>`; substituting `A := KioProduct<B, C>` leaves
`KioFunction1<KioProduct<B, C>, R>`, exactly the facade of a directly written
single product parameter `(B & C) -> R`. A stage with independently grouped
source parameters uses the corresponding larger `N`.

The unit value `()` flows as Rust `()` unchanged — no wrapping.

#### Representation witness API, safety, and coherence

The public core is generated schematically as follows (arity-specific forms
repeat the same contract):

```rust
pub struct KioStoredValue(/* private */);

pub trait KioType: __kio_type_private::Sealed + Clone + 'static {
    type Facade: Clone + 'static;
    fn into_stored(value: Self::Facade) -> KioStoredValue;
    fn from_stored(value: KioStoredValue) -> Self::Facade;
}

pub struct KioNative<T: Clone + 'static>(/* marker */);
pub struct KioValue<A: KioType>(/* private stored token and marker */);

impl<A: KioType> KioValue<A> {
    pub fn pack(value: A::Facade) -> Self;
    pub fn unpack(self) -> A::Facade;
    pub fn from_stored(value: KioStoredValue) -> Self;
    pub fn into_stored(self) -> KioStoredValue;
    pub fn stored(&self) -> KioStoredValue;
}
```

`KioNative<T>` is the emitted crate's local marker with `Facade = T`; it lets a
host instantiate a Kio kind-`*` binder with an arbitrary `T: Clone + 'static`
without implementing a foreign trait for a foreign type. `KioType` itself is
sealed. Its public implementations are the generated/local unit, bottom,
native, structural, function, application, nominal, and rank-N site marker
classes; the generated crate also has one private erased ingress marker whose
facade is `KioStoredValue`. That prevents
overlapping blanket implementations and avoids Rust's orphan/coherence traps.
It is compile-time machinery, not an object-safe interface, and emitted code
never forms `dyn KioType`. `KioTypeConstructorN` is likewise used only as a
generic bound; its generic `lift` / `project` methods need not be object-safe
and no `dyn KioTypeConstructorN` is formed.

`KioStoredValue` is nameable and `Clone`, but its raw erased-token constructor,
field, and downcast are crate-private. `KioType` and `KioValue<A>` deliberately
expose opaque-token transfer so an external invariant `DStorage` can pack and
unpack `A::Facade` values without knowing their native representation. A host
can therefore re-tag a token under the wrong marker; every such re-tag violates
the host storage contract. A representation-incompatible decode
deterministically panics. A representation-compatible decode may succeed, but
that possibility guarantees nothing about any marker pair and does not make
the re-tag valid. Generated safe Rust cannot cause undefined behavior. Values
kept under the matching marker do not encounter a representation-mismatch
panic; ordinary host operations such as `Clone` retain their own behavior.
External `KioTypeConstructorN` implementations may select any public, well-formed
`KioType` marker. An identity constructor can declare `Apply<A> = A`; a native
container constructor can declare
`Apply<A> = KioNative<Option<A::Facade>>`. `Apply` remains the exact public-
facade authority. Identity can inherit the safe conversion defaults. A
representation-varying container such as the native `Option` witness overrides
`lift` and `project` as a pair so its abstract carrier stores the substitution-
stable `Option<KioStoredValue>` representation. The conversion must be lossless,
preserve the marker association in both directions, and use only the documented
opaque-token API. The abstract `F(A)` occurrence remains `KioApplied1<F, A>`
with facade `KioApply1<F, A>` regardless of that selection.

#### Cost and source compatibility

Entering erased storage may allocate a reference-counted token; cloning a
stored value increments its reference count. Structural conversion follows
the binary spine. `KioValue<A>` lets a generated container retain the token
rather than expose or repeatedly guess its native type. The default `KioApplyN`
lift/project bridge invokes exactly one codec selected by the constructor's
`Apply` marker. A lawful override may instead translate element-wise between
the exact facade and substitution-stable opaque-token storage. This is typed
boundary conversion, not an HKT-specific body walk; the
`Option<KioStoredValue>` realization uses one explicit element-wise
`Option::map` in each direction. A token-backed nominal or application marker
can transfer the
existing token directly; a native marker may allocate on ingress and clone-
recover its value on egress, while a structural marker traverses its structure.
Host storage cloning costs whatever the selected `DStorage::clone` costs.

This facade replaces the earlier Rust spellings that used bare generic values,
`<F::Apply<...> as KioType>::Facade` as the value skin at an abstract
application occurrence, parameterized host GATs, and an occurrence-sensitive
`impl Fn` / `Rc<dyn Fn>` split. The current marker-valued `F::Apply<...>`
relation is distinct and remains part of `KioTypeConstructorN`. The replaced
spellings were emitted-crate APIs rather than Kio source semantics. A host
regenerating an affected crate
must update Rust source to pass marker arguments (`KioNative<T>` for an
ordinary native instantiation), use `A::Facade` in generic implementations,
wrap parameterized host storage through its declaration-owned carrier, and
construct/call functions through `KioFnN`. A `role(str)` host method now
receives its exact selected facade by value uniformly; an older implementation
whose parameter was `&Self::<StringType>` removes that reference.

## Package API

The `pub struct MyKiopkg<H: MyKiopkgHost>` returned from `create_myKiopkg`
exposes one field per bridged top-level module. Those fields lead to the
generated namespace handles described below.

### Exports

Each bridged module's `pub fn` becomes a method on that module's generated
namespace handle. The method's signature mirrors the source function
declaration, with Kio types mapped per the FFI surface above and any type
parameters becoming `A: KioType` marker generics. A value occurrence of such a
parameter is `A::Facade`; a native host call selects `KioNative<T>` when it
wants the facade value to be `T`. Thus `pub fn id[A](v: A) -> A` becomes a
namespace-handle method of the schematic form
`pub fn id<A: KioType>(&self, v: A::Facade) -> A::Facade`, the same marker
equation used by generic host-trait methods.

A bridged module's `pub` type is exported under its exact declaration path,
`shapes::nominal::<declaring module path>::<TypeName>`.
The type exists even when it has no public constructor or projector, so it
also serves as that newtype's per-type handle. Every newtype shares this rule;
there is no separate label-generated-vs-explicit namespace branch.

The exposed surface is **namespaced by the declaring module's path**:
the package surfaces a nested namespace field per path segment, so a
`pub fn answer` in module `pkg/main` is reached as `package.main.answer()`,
a `pub fn greet` in module `pkg/api` as `package.api.greet()`, and
`pkg/util/text` contributes methods under `package.util.text`. Because the
namespace is preserved, two `pub fn main`s in different bridged modules
(`a/main` and `b/main`) are distinct entries (`pkg.a.main.main()` and
`pkg.b.main.main()`); there is no leaf-name collision and no rename.

Each namespace field is a backend-emitted handle parameterized by
the same type `H`; its methods use the same FFI type mapping
as explicit `fn` methods.

### Newtype namespaces

Every public newtype has a declaration-specific
`shapes::nominal::<declaring module path>::<TypeName><__KioHost>` type,
whether it exports zero, one, or two members. Each `pub` constructor or
projector becomes an inherent
associated function; a non-`pub` member is absent. The resulting four
surfaces are:

| Public members | Rust boundary value |
| --- | --- |
| neither | nominal struct with hidden storage and no member methods |
| constructor only | nominal struct with hidden storage and only the constructor |
| projector only | nominal struct with hidden storage and only the projector |
| both | nominal struct with hidden storage and both selected methods |

For `newtype Foo : T { pub constructor mk_foo; pub projector
un_foo };`, a host calls

```rust
let f = my_kiopkg::shapes::nominal::app::model::Foo::<MyHost>::mkFoo(x);
let v = my_kiopkg::shapes::nominal::app::model::Foo::<MyHost>::unFoo(f);
```

Each selected method retains the exact declared Kio member scheme after the
Rust boundary mapping, including universal generics and function-valued
payloads. Every kind-`*` binder is a `KioType` marker. A kind-`*→…→*` binder
uses `KioTypeConstructorN`; an occurrence `F(A, …)` is
`KioAppliedN<F, A, …>` with facade `KioApplyN<F, A, …>`. The constructor
witness's associated `Apply<A, …>: KioType` and public `lift` / `project`
methods relate that abstract carrier to the exact declaration-owned marker;
the public signature never relies on `F::Apply` being the facade value itself.
An existential projector remains CPS-shaped;
each hidden binder is represented by a declaration-and-slot-owned sealed
`KioType` witness whose facade is opaque to the caller, so repeated uses retain
one exact identity without exposing `KioStoredValue`. Canonical Unit contributes no runtime
argument and uses a nullary continuation. No surface exposes a raw field or
tuple-struct constructor.

A parametric newtype threads each source kind-`*` parameter as a `KioType`
marker, not as an unconstrained Rust value type. Its declaration-owned marker
has `Facade = shapes::nominal::<path>::<Name><H, A, ...>`, and its public
constructor/projector payload uses `A::Facade` recursively. This same nominal
marker is used at direct, substituted, recursive, and retained exact
occurrences. Under an abstract constructor binder, `KioAppliedN` / `KioApplyN`
is used instead, and `lift` / `project` relates that carrier to this exact
nominal marker. Member visibility changes capabilities, not identity.

The carrier identity is the exact declaring module plus newtype name.
Two same-leaf declarations in different modules therefore remain different
Rust types. Nominal identities are declaration-keyed inside
`shapes::nominal`; anonymous products and sums use the fixed canonical binary
markers/facades. Per-boundary `ffi` aliases name those same types. Neither
naming process consults an occupancy or declaration-order claim schedule.

This namespacing mirrors the JS backend's `pkg.<TypeName>.<member>`
([`js.md` § Explicit newtype namespaces](js.md#explicit-newtype-namespaces)).
Hosts crossing backends see the same per-newtype namespace in every
language.

A host that finds the `::<MyHost>` turbofish noisy may introduce a
type alias at its call boundary:

```rust
type Foo = my_kiopkg::shapes::nominal::app::model::Foo<MyHost>;

let f = Foo::mkFoo(x);
let v = Foo::unFoo(f);
```

## Higher-rank value parameters

A fn whose value-parameter type carries a `[T]` quantifier inside a
function-typed slot — `fn if_c[A](b: [T](T & T) -> T, x: A, y: A) -> A {
b(A, x, y) }` — is a *rank-N value parameter*. Rust has no closure type with
a type-generic `Fn::call`; the generated exact facade therefore owns two
site-specific items. `KioForall_<exact-site>` is the host implementation trait
whose generic `apply<T: KioType, ...>` method retains the type binders and uses
`T::Facade` at value occurrences. `KioForall_<exact-site>Value` is the public
`Clone` carrier around one private stored body token and implements that trait
by marker-directed conversion. An exact generated sealed `KioType` marker for
the site has `Facade = KioForall_<exact-site>Value`, so the complete
polymorphic scheme can itself occur inside a product, sum, function, newtype,
type argument, recursive payload, or existential payload. Its final ordinary
function stage uses `KioFnN`.

Construction ingress accepts the canonical named carrier at every occurrence.
Host code turns `impl KioForall_<exact-site> + 'static` into that carrier with
`KioForall_<exact-site>Value::new(implementation)` before passing a direct
boundary slot or placing the value inside a product, sum, function, newtype,
type argument, recursive payload, or existential payload. Egress returns the
same `Value` carrier. The stable `ffi::<env|exp>::<member>` namespace names each
non-Unit, nameable ordinary parameter as `argN` and return as `ret`; public
newtype members instead use `<member>_argN` for constructor inputs and
`<member>_ret` for projector outputs. Every callable slot alias additionally
exposes `<slot>_cbarg` for one source parameter or `<slot>_cbarg0`,
`<slot>_cbarg1`, ... for several, plus `<slot>_cbret`; Unit slots remain
omitted. This applies to argument and result aliases, to ordinary functions
and forall carriers, and again when a callback parameter or result is itself
callable. A forall carrier pairs its slot alias with `<slot>_impl`. Its
callback aliases introduce the callable's type binders after the enclosing
binders and retain only the parameters referenced by the aliased type.
An existential projector instead exposes `<member>_continuation` and
`<member>_continuation_impl`; the same callback suffixes name its payload
parameter and result, including a polymorphic payload's carrier and `_impl`.
Each alias retains the referenced host parameter first, then source-marker
parameters in declaration order. Host code therefore does not reconstruct an
exact-site spelling. The
implementation trait is never made into `dyn KioForall_<exact-site>` — its
generic method is intentionally not object-safe. Site ownership keeps the
complete binder, structural, function, and result relation in the generated
Rust type while the value carrier adapts it to the erased staged body.

**Erased storage.** Erasing `T` does not flatten its `Forall` into the value
parameter list. The body stores a staged callable: applying the type binder
advances to the callable's next stage without accepting a term-level type
value, and the later function layer accepts the erased value parameters.
Every binder of an escaped value remains one callable stage: a work-free stage
returns the next callable, while a stage with computation performs it first.
A direct statically known call may compact adjacent leading type applications,
but computation cannot move past a later type or value application.

**Construction and invocation.** A lambda literal targeting a rank-N slot
(for example, `fn t() -> [A] (A & A) -> A { .[A](x, _y) { x } }`) is lowered
from its canonical nested `Forall` / `Function` tree. A call such as
`b(A, x, y)` advances the type stage and then calls the value stage; `A`
itself produces no Rust argument. Direct calls and computed rank-N values
follow the same tree, so a returned polymorphic value retains any preceding
computation rather than being flattened into its consumer's value call.

**Why private erasure is observably correct.** A Kio rank-N value is
binder-uniform: the type system rejects any operation whose meaning depends on
a specific instantiation of `[T]`. The body therefore uses one erased value
representation at every instantiation, while the public callable trait and
the staged adapter preserve the exact binder relation and source application
order. The erased representation does not occur in the public signature.

This is per-backend codegen, not a spec carve-out. JavaScript preserves the
same staged-callable structure while erasing binder values entirely; Rust
preserves it with crate-private raw erased body values and opaque
`KioStoredValue` boundary carriers. Kio's surface
semantics (sound, decidable type system; binder-uniform polymorphic body) apply
identically across backends.

## Higher-kinded types

The Rust backend is **type-erased internally** (§ Language families): Kio's
higher-kinded application carries no additional *body* representation. The
public facade nevertheless represents the complete type relation explicitly.
Every saturated type is a `KioType` marker; each kind-`*→…→*` binder is a
`KioTypeConstructorN` witness; and every abstract occurrence `F(A1, …, An)`
uses `KioAppliedN<F, A1, …, An>` whose facade is
`KioApplyN<F, A1, …, An>`. Rust never uses `F::Apply<...>` as the abstract
facade at some occurrences and `KioApplyN` at others. An external witness may
select any public, well-formed `KioType` marker for its exact `Apply` relation,
including `KioNative<Option<A::Facade>>`; that selection changes neither the
abstract marker nor its `KioApplyN` facade.

For arity one, the public relation is schematic as follows:

```rust
pub trait KioTypeConstructor1: Clone + 'static {
    type Apply<A: KioType>: KioType;

    fn lift<A: KioType>(
        value: <Self::Apply<A> as KioType>::Facade,
    ) -> KioApply1<Self, A> {
        /* generated default packs through Self::Apply<A> */
    }

    fn project<A: KioType>(
        value: KioApply1<Self, A>,
    ) -> <Self::Apply<A> as KioType>::Facade {
        /* generated default projects through Self::Apply<A> */
    }
}

pub struct KioApplied1<F: KioTypeConstructor1, A: KioType>(/* marker */);
pub struct KioApply1<F: KioTypeConstructor1, A: KioType>(/* private token */);
```

`KioApplied1<F, A>` implements `KioType<Facade = KioApply1<F, A>>`. A
declaration-owned constructor marker chooses its exact generated marker in
`Apply<A>`; an external witness may choose any public marker satisfying the
same bound. The public trait supplies safe default `lift` / `project` methods
derived from the selected marker's codec; they bridge between the
exact facade and the abstract `KioApply1` carrier. The `KioApply1` field and
raw `Any` construction/downcast stay private; external host storage may move
the public opaque `KioStoredValue` only through the documented
`KioType::{into_stored, from_stored}` codec or its typed `KioValue` wrapper.
The default bridge invokes the exact `Apply` marker's codec once. An external
constructor whose exact facade representation varies with its arguments may
override both methods with a lossless, substitution-stable opaque-token
representation. `Apply` and the method signatures remain the exact public-
facade authority, and every abstract occurrence remains `KioApply1`. Such an
override may perform element-wise typed boundary conversion without adding an
HKT-specific body walk. Token-backed nominal and application markers transfer
the existing token directly. In particular, body erasure is not a proof that
Rust can recover an arbitrary native GAT equality.

The two associated names have different jobs: `F::Apply<A>` is a `KioType`
*marker* naming the concrete constructor's exact application, while
`KioApply1<F, A>` is the Rust *value facade* used for every occurrence under
the abstract binder `F`. `lift` and `project` are the codec bridge between
them. Code never alternates between `F::Apply<A>::Facade` and `KioApply1` for
the same abstract occurrence according to context.

A host-nameable partial nominal whose remaining declaration parameters are all
kind `*` uses a declaration-owned constructor witness for precisely its
remaining arity. If a declaration of total arity `n` has an admissible prefix
of `p < n` supplied arguments, the public witness has the schematic name
`KioNewtypeConstructor_<declaration-identity>_P<p><H, captured...>` or
`KioHostTypeConstructor_<declaration-identity>_P<p><H, captured...>` and
implements `KioTypeConstructor{n-p}`. Its `Apply` relation appends the remaining
markers and yields the same exact saturated declaration marker used by a direct
nominal application. Captured higher-kinded parameters retain their matching
constructor bounds; a partial form whose *remaining* parameter is itself
higher-kinded has no `KioTypeConstructorN` witness because that trait consumes
only kind-`*` marker arguments. The declaration identity, prefix length, and
captured marker sequence alone determine each admitted name and relation;
unrelated declarations cannot change it.

The dictionary that underwrites a `Functor` / `Monad` instance is an
**ordinary value Kio threads explicitly** — Kio has no typeclasses, so an
instance is a `fn`-parameter value, never a host typeclass instance (see
[`README.md` § Higher-kinded types](README.md#higher-kinded-types)). The body
builds and applies it as an erased value; the generated
constructor/application markers and rank-N callables are boundary declarations,
not runtime HKT machinery.

## Polymorphic newtype payloads

A newtype whose payload is itself a polymorphic function — the dictionary pattern that underwrites `Functor[*F]`, `Monad[*F]`, and related abstractions — stores the complete staged callable in the body's universal erased representation. See [`backends/README.md` § Polymorphic newtype payloads](README.md#polymorphic-newtype-payloads) for the cross-cutting scheme; this section describes the Rust concretization.

```kio
pub newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B) {
  pub constructor mk_monad;  pub projector bind;  };
```

The payload's public type uses `KioApplied1<F, A>` /
`KioApplied1<F, B>` markers and their `KioApply1<F, ...>` facades throughout,
plus a site-owned callable trait whose generic methods bind `A: KioType` and
`B: KioType`. Its final value function is the corresponding `KioFnN` facade.
Its private value uses the erased body representation. The adjacent binder run
remains an ordered pair of hidden callable stages, each returning the next
callable when it has no work.
The following value function remains a separate callable stage. A direct known
call may compact its leading type applications, but this stored first-class
payload may not.

**Stored generic values.** A generated container that retains a value of
abstract type `A` stores `KioValue<A>`, not `A::Facade` behind a guessed native
container equality. A polymorphic map-like operation unpacks through `A`, calls
its `KioFn1<A, B>` with `A::Facade`, and packs the `B::Facade` result as
`KioValue<B>`. The same rule applies when `A` or `B` is structural, nominal,
functional, existentially witnessed, or an abstract constructor application;
the marker algebra selects the conversion and the container keeps only the
opaque token.

**Constructor / projector.** The dictionary newtype keeps its erased staged
payload in private carrier storage. `mk_monad` and `bind` adapt the exact
public callable trait at that boundary without flattening its stages:

```rust
// public exact callable ↔ private staged callable ↔ nominal carrier
```

A `bind` call first performs the projected payload's type applications and
then invokes its stored value function. The projector neither forces nor
flattens those stages. The generated constructor witness's default lift/project
invokes the selected application marker's codec once while preserving that
exact marker, with no separate HKT body walk. A token-backed nominal or
application codec transfers the existing private token directly; other codecs
retain their declared
allocation, clone, or structural-conversion cost.

**Erased-mode body lowering at the construction site.** When a Kio lambda literal flows into a polymorphic-newtype-payload constructor — `Monad.mk_monad(Box, .[A][B](m, k) { k(Box.un_box(m)) })` — the bound types and value parameters use the universal representation while the nested callable stages follow the payload type. The concrete constructor's generated adapter performs its marker-directed lift / project while the body retains the opaque token. This behaves identically to a typed-throughout version because the typer rejects any operation that depends on a specific instantiation of `[A]` or `[B]` (binder-uniformity).

**Multi-arity abstract type-constructor binders.** The scheme is uniform
across arity: a dictionary that quantifies over `[**F]` uses
`KioApplied2<F, A, C>` and `KioApplied2<F, B, D>` as the saturated markers,
whose facades are the matching `KioApply2` carriers. The constructor witness's
associated `Apply<A, C>: KioType` names only the exact declaration side of its
lift/project seam. The body keeps the same staged erased representation. The
emitter declares only the witness arities present in the frozen catalog.

## Item naming

Source-derived public Rust identifiers use
[shared word casing](README.md#source-derived-public-names) (`do_work` → `doWork`,
`Native_token` → `NativeToken`). Rust-reserved words require escaping, and
several lexical type binders sharing one Rust generic scope require
alpha-renaming. A keyword uses Rust's `r#`
raw-identifier escape when legal; the strict reserved spellings that Rust
forbids as raw identifiers use the backend's reserved `__kio_kw_` escape. A
host calling a Kio fn named `match` calls `pkg.r#match()`. For a repeated type
binder spelling, the first binder keeps the readable source spelling and each
later binder uses a deterministic `__KioType_<binder identity>` spelling. Kio
type identifiers cannot begin `__`, so that class is outside the image of
source names; declaration and use sites share the same semantic-binder map.

Newtype-member access is namespaced under the emitted newtype struct.
For a `Foo` declared in `app/model`, `Foo.mk_foo` becomes
`shapes::nominal::app::model::Foo::<MyHost>::mkFoo`, not a flattened
`pkg.foo_mk_foo`.
See § Newtype namespaces above. The
namespacing brings Rust in line with the JS backend's
`pkg.<TypeName>.<member>` shape.

Every nominal declaration uses the same nested path regardless of member
visibility or package occupancy:
`shapes::nominal::<escaped module segment>…::<escaped TypeName>`.
Each Kio module segment becomes one word-cased Rust module segment, followed
by Rust-keyword escaping. The type leaf follows the same rule.
There are no flat aliases, module-path concatenations, collision hashes, or
visibility-dependent carrier spellings.

Source-readable nominal constructor, type-marker, host-carrier, callable-site,
and `forall` owner frames word-case each source component before escaping
remaining `_` as `_u` and `/` as `_s`. Fixed role prefixes and indexed suffixes
remain fixed: a member `mk_foo` contributes `mkFoo_arg0` / `mkFoo_ret` slot
aliases. Raw nominal identities and opaque structural identity encodings
are not word-cased.

Fixed algebra names are `KioUnit`, `KioBottom`, `Product`, `Sum`, `KioType`,
`KioNative`, `KioValue`, `KioStoredValue`, `KioProduct`, `KioSum`, and the numeric
`KioFunctionN` / `KioFnN`, `KioTypeConstructorN`, and
`KioAppliedN` / `KioApplyN` families. Exact newtype and host-type markers,
constructor witnesses, and parameterized-host facades encode the complete
qualified declaration identity injectively. Word-case each source component
of the module path and declaration leaf, then escape remaining `_` as `_u`
and `/` as `_s`; the two encoded components are joined by
`__`. If `q` is that joined encoding, their templates are `KioNewtypeMarker_<q>`,
`KioHostTypeMarker_<q>`, `KioNewtypeConstructor_<q>_P<p>`,
`KioHostTypeConstructor_<q>_P<p>`, and `KioHostType_<q>`. The `P<p>` suffix is
the number of already supplied declaration parameters. Schematic names in
this page such as `KioHostType_app__Array` stand for the corresponding encoded
spelling. Marker, facade, constructor-witness, and storage roles use disjoint
prefixes, so adding another declaration cannot capture an existing support
name.

## Worked example

```kio
// hello.pkg.kio
package hello;

bridge {
  hello;
  hello/**;
}
```

```kio
// hello.kio
module hello;

host type I32 role(i32);
host type String role(str);

host fn print(p0: String) -> .;
host fn int_to_string(p0: I32) -> String;
```

```kio
// hello/main.kio
module hello/main;

import hello(I32, String, print, int_to_string);

pub labels { hed: I32, mid: String };

pub fn main() -> . {
  let r = ({hed = 42(I32)}, {mid = "hi"(String)});
  print(int_to_string(r.?{hed}));
  print(", "(String));
  print(r.?{mid})
}
```

Emitted Rust skeleton (illustrative; declaration-owned support names use the
exact identifier encoding described above):

```rust
// src/host.rs — every nullary host type is an exact associated type. A role
// adds its standard From capability; it does not choose the type.
// Every role(str) occurrence uses the exact associated type by value.
// Method and associated-type names carry the declaring module's
// namespace (`hello__…`).
pub trait HelloHost: Clone + 'static {
    type hello__I32: Clone + PartialEq + 'static + From<i32>;
    type hello__String: Clone + PartialEq + 'static + From<String>;

    fn hello__print(&self, s: Self::hello__String);
    fn hello__intToString(&self, v: Self::hello__I32) -> Self::hello__String;
}

// src/shapes.rs — shape struct fields hold the exact associated types by
// value. `Hed` and `Mid`
// are label-generated in module `hello/main`, so their nominal facade
// mirrors that declaring path.
pub mod nominal {
    pub mod hello {
        pub mod main {
            pub struct Hed<__KioHost: 'static> { /* private storage */ }
            impl<__KioHost: HelloHost> Hed<__KioHost> {
                pub fn mk(payload: __KioHost::hello__I32) -> Self { … }
                pub fn get(value: Self) -> __KioHost::hello__I32 { … }
            }

            pub struct Mid<__KioHost: 'static> { /* private storage */ }
            impl<__KioHost: HelloHost> Mid<__KioHost> {
                pub fn mk(payload: __KioHost::hello__String) -> Self { … }
                pub fn get(value: Self) -> __KioHost::hello__String { … }
            }
        }
    }
}

pub struct Product<A, B> {
    pub _0: A,
    pub _1: B,
}

// src/lib.rs
pub mod host;
pub mod shapes;

pub struct Hello<H: HelloHost> {
    // one namespace field per bridged module path segment;
    // `pkg.hello.main.main()` reaches the export.
    pub hello: HelloNs<H>,
}

pub fn create_hello<H: HelloHost>(host: H) -> Hello<H> { … }

impl<H: HelloHost> HelloMainNs<H> {
    pub fn main(&self) {
        // Internal representation + per-signature wrappers omitted for
        // brevity.
    }
}
```

A host invokes the package via `let pkg = hello::create_hello(MyHost); pkg.hello.main.main()`. If the host wants to construct a `Hed` directly (e.g., when calling a fn that takes one), it goes through the namespaced constructor:

```rust
let h = hello::shapes::nominal::hello::main::Hed::<MyHost>::mk(42_i32.into());
```

Or with a type alias for brevity:

```rust
type Hed = hello::shapes::nominal::hello::main::Hed<MyHost>;
let h = Hed::mk(42_i32.into());
```

# Hosting Kio in Rust

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to Rust and calling into it from a Rust host. The authoritative contract is [`specs/backends/rust.md`](../../specs/backends/rust.md); this guide is the narrative companion.

> **Known host-source compatibility caveat.** Removing a bridged `host type`
> requires an existing host to delete the stale associated-type entry from its
> host-trait `impl`. See the Rust contract's
> [host-type removal section](../../specs/backends/rust.md#host-type-removal-is-not-source-stable-on-rust).
> When retained signatures require incompatible epochs of one exact nominal
> declaration, Rust also omits the affected trait methods and types; existing
> host source must delete or rewrite those trait items and generated type
> references. See the contract's
> [incompatible-retained-epochs section](../../specs/backends/rust.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-rust).

## The build target

A package emits to Rust by declaring a `rust` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target rust {
    out "out/rust/";
    namespace "greeter"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`namespace` sets the generated `Cargo.toml` name field; if absent it defaults to the Kio package name.

Artifact and public names are distinct: namespace `my_pkg` keeps crate
`my_pkg`, with handle `MyPkg`, host trait `MyPkgHost`, and factory
`create_myPkg`. The fixed `create_` prefix is retained. Affixed source names,
reserved defaults, and non-source explicit overrides use the exact brands
specified in [the namespace rules](../../specs/backends/rust.md#output-layout).

`kio build` or `kio build rust` writes a self-contained Cargo crate under `out/rust/`:

```text
out/rust/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── host.rs
    ├── shapes.rs
    ├── ffi.rs
    └── __kio_runtime.rs
```

`src/lib.rs` is the package surface, `src/host.rs` carries the host trait the
host implements, and `src/shapes.rs` holds Rust items for values that cross
the FFI. It includes the `KioType` representation markers, canonical binary
`Product` / `Sum` values, named `KioFnN` callables, abstract `KioApplyN`
carriers, and nominal types mirrored under `shapes::nominal`. The erased value
token is opaque; host code works through these typed values.

## Loading the package

A Rust host brings the package online in three steps.

### 1. Add the crate as a dependency

```toml
[dependencies]
greeter = { path = "./out/rust" }
```

### 2. Implement the host trait

The generated `GreeterHost` trait declares one required method per live
`host fn` in the bridged modules, plus any optional deprecated retained
defaults described under [Removed host declarations](#removed-host-declarations).
Method names carry the module namespace. Paths without `_` replace `/` with
`_`; paths containing `_` use a reserved `__kio_host_` prefix, word-case each
component, then escape remaining `_` as `_u` and `/` as `_s`. The separator
`__` precedes the word-cased item name: `data_api.do_work` becomes
`__kio_host_dataApi__doWork`. So a `host fn print` in module `greeter` becomes `greeter__print`:

```kio {variant=module}
// greeter.kio
module greeter;

host type String role(str);

host fn print(p0: String) -> .;
```

```rust
use greeter::host::GreeterHost;

#[derive(Clone)]
struct MyHost;

impl GreeterHost for MyHost {
    type greeter__String = String;

    fn greeter__print(&self, s: Self::greeter__String) {
        print!("{s}");
    }
}
```

Every nullary `host type` is an associated type on the generated Rust trait,
and its module-qualified name preserves the Kio declaration's identity. A role adds a
standard construction bound rather than choosing the representation: here
`role(str)` requires `greeter__String: From<String>`, so the host may select
`String` or a local wrapper that implements that trait. Every string
occurrence uses that selected type by value: direct parameters, returns,
stored values, structures, and callbacks all share one facade. The host can
borrow it inside its implementation when useful. A literal is accepted only when its lexical kind matches the selected
declaration's role, then is constructed through that exact type's `From`
implementation. A mismatched role or a roleless host type cannot be a literal
annotation or contextual literal type.

A parameterized host type uses a different surface. Stable Rust can express a
declaration-scoped generic associated type, but that does not enforce the
substitution-stable application/storage relation required when an arbitrary
Kio constructor flows through erased polymorphic code. For example, a
declaration `host type Array[A];` adds one
`type greeter__ArrayStorage: Clone + PartialEq + 'static;` entry, plus a
generated `KioHostType_greeter__Array<Self, A>` facade for each marker
`A: KioType`. Schematically, that facade has:

```text
pub fn from_storage(value: H::greeter__ArrayStorage) -> Self;
pub fn storage(&self) -> H::greeter__ArrayStorage;
pub fn into_storage(self) -> H::greeter__ArrayStorage;
```

`storage()` recovers a clone of the host's selected storage. `into_storage()`
also recovers a clone, after consuming the carrier, because the opaque token
may be shared. Both operations perform a constant-time token downcast followed
by the host-selected storage's clone cost. The host keeps the invariant that
its storage contains the value for the carrier's declared application.
`KioValue<A>` exposes typed pack/unpack and opaque-token
transfer so an external storage container can retain generic elements. A host
can deliberately put a token back under the wrong marker; every such re-tag
violates the host contract. An incompatible representation deterministically
panics when decoded. A compatible representation may decode successfully, but
that possibility is not a guarantee for any marker pair and does not make the
re-tag valid. The raw erased value and downcast operation remain private, and
generated safe Rust cannot cause undefined behavior. This
storage carrier is used for every parameterized declaration, including
`Box[A]`- or `Array[A]`-shaped host types; only nullary host atoms use their
native associated type directly.

### 3. Instantiate and invoke

```rust
let pkg = greeter::create_greeter(MyHost);
pkg.greeter.main.main();
```

`create_greeter` returns a `Greeter<MyHost>` whose surface mirrors the bridged exports under their module namespace: an export `main` from module `greeter/main` is reached as `pkg.greeter.main.main()`. The host value is moved into the package and owned for the package's lifetime.

Source-derived module, type, function, constructor, and projector components
use word casing (`my_module` → `myModule`, `Native_token` → `NativeToken`,
`mk_pair` → `mkPair`), preserving initial case and outer underscore runs.
Rust keywords then use the documented raw or reserved identifier escape.
Source-readable carrier and slot frames use the same conversion while keeping
their fixed role and index suffixes; semantic nominal identity is unchanged.

## Generic values and functions

Rust distinguishes a generic Kio *type marker* from the Rust value that
represents it. A generated generic signature binds `A: KioType` and uses
`A::Facade` for each value of type `A`. At a concrete host call,
`KioNative<T>` selects the ordinary Rust facade `T`:

```text
use greeter::shapes::KioNative;

let result: String = pkg.greeter.api.id::<KioNative<String>>("hi".to_owned());
```

`KioType` is sealed, so hosts do not implement it for their own types;
`KioNative<T>` is the local adapter. Generated containers use `KioValue<A>`
when they need to retain a generic value in opaque storage.

A unique source type-parameter name stays readable in generated Rust. If
lexically distinct Kio binders reuse a spelling and are combined into one Rust
generic scope, later binders receive deterministic reserved names while their
positions and references stay unchanged. Host code should use the exact
generic spelling shown by the generated signature or its stable `ffi` alias.

Function values likewise have one stable public spelling everywhere. A unary
`A -> B` value is `KioFn1<A, B>`, constructed from and invoked like this:

```text
use greeter::shapes::{KioFn1, KioNative};

let add_one = KioFn1::<KioNative<i32>, KioNative<i32>>::new(|x| x + 1);
assert_eq!(add_one.call(41), 42);
```

The same named callable is used in parameters, returns, products, sums, and
newtype payloads, so substituting a generic function type never changes its
Rust representation. The number in `KioFnN` counts the source value stage's
parameters, not the contents of a product parameter: substituting
`A := B & C` in `A -> R` remains one `KioFn1` argument whose facade is a
`Product`.

A rank-N value (`[A] ...`) has a site-owned
`KioForall_<site>` implementation trait with a generic
`apply<A: KioType>` method. Host code supplies an ordinary struct implementing
that trait, then calls the generated
`KioForall_<site>Value::new(implementation)` constructor. The same cloneable
`Value` carrier is used for direct parameters, returns, and occurrences inside
another value, so substitution never changes its Rust representation. Stable
`ffi::<env|exp>::<member>` aliases name every non-Unit, nameable ordinary
parameter as `argN` and return as `ret`; newtype members instead use
`<member>_argN` and `<member>_ret`. A callable alias also exposes
`<slot>_cbarg` (or indexed `<slot>_cbarg{i}`) and `<slot>_cbret`, omitting
Unit types. These names work for returned callbacks as well as inputs and
repeat when a callback parameter or result is itself callable. A forall
carrier pairs its slot alias with `<slot>_impl`; an existential projector uses
the pair `<member>_continuation` / `<member>_continuation_impl`. For example,
`<member>_continuation_cbarg` names the payload parameter in the continuation's
generic `apply` method, and a polymorphic payload also has
`<member>_continuation_cbarg_impl`. The aliases retain the host parameter first
when needed, followed by the enclosing and callback type markers they
reference, in declaration order. Host code does not need to copy a generated
site name. Rust closures cannot
have a type-generic call method, and the generated API does not try to make
this trait into a `dyn` object.

The constructor-witness trait is intentionally not sealed, so a host can
compose the generated marker algebra. Identity needs only its marker equation;
the trait supplies the safe opaque-token bridge:

```text
#[derive(Clone)]
struct Identity;

impl KioTypeConstructor1 for Identity {
    type Apply<A: KioType> = A;
}

#[derive(Clone)]
struct Optional;

impl KioTypeConstructor1 for Optional {
    type Apply<A: KioType> = KioNative<Option<A::Facade>>;

    // The abstract carrier stores `Option<KioStoredValue>` independently of
    // `A`. `lift` converts typed facades to tokens; `project` converts them
    // back. These are boundary conversions, not validation or serialization;
    // each direction uses one explicit `Option::map`.
    fn lift<A: KioType>(value: Option<A::Facade>) -> KioApply1<Self, A> {
        let payloads = value.map(|value| KioValue::<A>::pack(value).into_stored());
        let stored = KioValue::<KioNative<Option<KioStoredValue>>>::pack(payloads)
            .into_stored();
        KioValue::<KioApplied1<Self, A>>::from_stored(stored).unpack()
    }

    fn project<A: KioType>(value: KioApply1<Self, A>) -> Option<A::Facade> {
        let stored = KioValue::<KioApplied1<Self, A>>::pack(value).into_stored();
        let payloads = KioValue::<KioNative<Option<KioStoredValue>>>::from_stored(stored)
            .unpack();
        payloads.map(|value| KioValue::<A>::from_stored(value).unpack())
    }
}
```

Because `KioType` itself is sealed, an external constructor's `Apply` must be
an existing generated/local marker. It may select any public, well-formed
marker, including a `KioNative` native-container marker such as the `Optional`
example. `Apply` remains the exact public-facade authority. The default
`lift` / `project` methods use that selected marker's codec. A representation-
varying container overrides the pair with a lossless, substitution-stable
opaque-token representation, as `Optional` does above. An abstract `F(A)`
nevertheless remains the `KioApplied1<F, A>` marker with `KioApply1<F, A>` as
its facade; choosing the exact marker does not make the native container the
abstract facade.

These marker/facade spellings replace older generated Rust APIs that used bare
generic values, `<F::Apply<...> as KioType>::Facade` as an abstract
application's value skin, parameterized host GATs, or different closure types
depending on occurrence. The current marker-valued `F::Apply<...>` relation is
part of `KioTypeConstructorN` and is not that old value skin. After regenerating an
affected crate, update host source to select `KioNative<T>`, write generic
values as `A::Facade`, wrap a parameterized declaration's associated `Storage`
value in its declaration-owned carrier, construct/call functions through
`KioFnN`, and accept `role(str)` values by value rather than by reference.
Entering opaque storage may allocate a
reference-counted token; cloning that token increments its reference count.
Default application lift/project invokes the selected `Apply` marker codec
once. A lawful override may instead translate element-wise between the exact
facade and stable opaque-token storage; the `Optional` example uses one
explicit element-wise `Option::map` in each direction. This typed boundary
conversion is not a separate HKT-specific body walk. A token-backed marker transfers the token
directly; native and structural markers perform their ordinary codec work.

## Newtypes

Every public newtype in a bridged module has a generated type at
`shapes::nominal::<declaring module path>::<TypeName>`, even when it has no
public constructor or projector. For example, `newtype Token` declared in
`greeter/model` is `greeter::shapes::nominal::greeter::model::Token<MyHost>`.
Its public surface follows the members the package exposes:

- With no public members, it is an opaque Rust type.
- With only a public constructor, the host can create values but cannot
  inspect them.
- With only a public projector, the host can inspect values received from the
  package but cannot create them.
- With both public, the carrier exposes both selected methods while its
  storage remains private. Recursive and mutually recursive values keep this
  same exact nominal carrier; they never fall back to a public erased type.

Every form keeps storage private. Its methods retain universal generic and
function types after Rust's normal boundary mapping. A parameterized newtype
threads `A: KioType` through its nominal carrier and uses `A::Facade` in the
constructor/projector payload, so direct and substituted occurrences retain
one declaration-owned Rust type. A higher-kinded binder
uses a generated `KioTypeConstructorN` witness. Every abstract application is
the `KioAppliedN<F, ...>` marker with a `KioApplyN<F, ...>` facade; the
declaration-owned witness's `lift` / `project` methods bridge it to the exact
newtype marker. The surface never asks Rust to equate an arbitrary
`F::Apply<...>` native type: `Apply` names the exact `KioType` marker, while
`KioApplyN` is the value passed at an abstract occurrence. An existential
projector accepts a continuation over declaration-and-slot-owned opaque witness
types and returns the continuation's result; the erased storage behind those
witnesses is not public. Canonical Unit contributes no runtime argument, so
that continuation is nullary.

Partially applying a known multi-parameter host type or newtype produces a
declaration-owned constructor witness when every remaining parameter has kind
`*`. Its generated name includes `_P<p>`, where `p` is the number of captured
prefix arguments, and its generic arguments carry those exact markers (with
constructor bounds retained for captured higher-kinded parameters). Applying
the remaining kind-`*` arguments reaches the same saturated declaration carrier
as a direct application; hosts do not invent a native constructor equality for
the partial form.

Each generated type belongs to one exact module-and-newtype declaration, so
same-named newtypes in different modules remain distinct. When another
boundary type contains an opaque or one-member newtype, it contains that type
as one value rather than exposing its hidden payload. The nested path is used
uniformly for zero-, one-, and two-member surfaces and depends only on that
declaration. Adding another newtype cannot rename an existing host type.
Anonymous structural values use the canonical binary product/sum types at
`shapes` root. Their signature type is always the canonical right-nested
binary facade: `A & B & C` is
`Product<A::Facade, Product<B::Facade, C::Facade>>`. Stable per-boundary `ffi`
aliases may name that same type; Rust emits no competing semantic-keyed wrapper.
This matters for generic code: substituting `T := B & C` in `A & T` produces
the identical Rust facade, not an outer two-slot wrapper around a competing
flat three-slot type.

## Removed host declarations

A removed host function whose frozen signature remains nameable is preserved
as a `#[deprecated]` trait method with a trapping default. New hosts may omit
it, and generated package code has no route to call it. Public aliases,
nominal carriers, representation and constructor witnesses, rank-N witnesses,
and site/declaration-owned support reached only from that history are
deprecated too; a live use keeps a shared declaration live and nondeprecated.
The canonical `Product` / `Sum` algebra itself is shared support and does not
become history-only.

Rust cannot retain an optional associated entry on stable Rust. After a
nullary `host type` is removed, an existing host must delete its stale facade
associated-type definition; after a parameterized one is removed, it deletes
the stale `Storage` definition. If a removed function's signature mentions that removed type, its
deprecated method is omitted as well, and an existing host must delete the
stale method override. This avoids imposing any history-only associated type
or method on a new host. See the matching limitation and compiler evidence in
[`specs/backends/rust.md` § Deprecated host items](../../specs/backends/rust.md#deprecated-host-items).

If sealed history contains incompatible declarations at one exact nominal
identity, no generated Rust type can preserve both frozen signatures. The
generator omits the affected retained trait methods and type closure. Existing
host source must delete or rewrite those trait items and direct generated-type
references; current implementations remain live-only. See the caveat banner
above.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target rust {
    out "out/rust/";
    namespace "greeter"
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

  target rust {
    out "out/rust/";
    namespace "greeter";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_rust_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_rust_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() -> . { print("hello from kio\n") }
```

Build and run:

```sh
kio build rust
```

```rust
use greeter::host::GreeterHost;

#[derive(Clone)]
struct MyHost;

impl GreeterHost for MyHost {
    type greeter__String = String;

    fn greeter__print(&self, s: Self::greeter__String) {
        print!("{s}");
    }
}

fn main() {
    let pkg = greeter::create_greeter(MyHost);
    pkg.greeter.main.main();
}
```

## Where this leads

- [`specs/backends/rust.md`](../../specs/backends/rust.md) — the full Rust backend contract.
- [`specs/backends/README.md`](../../specs/backends/README.md) — cross-cutting properties shared by every backend.
- [Getting started with the Kio tooling](../tutorials/tooling.md) — build and check a package before loading it in a host.
- [`specs/cli.md`](../../specs/cli.md) — the exact `kio build` command contract.

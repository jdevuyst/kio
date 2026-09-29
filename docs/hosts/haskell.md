# Hosting Kio in Haskell

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to Haskell
and calling into it from a Haskell host. The authoritative contract is
[`specs/backends/haskell.md`](../../specs/backends/haskell.md); this guide
is the narrative companion.

> **Known host-source compatibility caveat.** Removing a bridged `host fn`
> changes the generated Haskell record, so an existing host must remove that
> field from its `<Handle>Host { … }` literal. See the Haskell contract's
> [host-fn removal](../../specs/backends/haskell.md#host-fn-removal-is-not-source-stable-on-haskell)
> section.
> When retained type declarations require incompatible epochs of one exact
> nominal identity, Haskell also omits the affected compatibility equations
> and types; existing host source must delete or rewrite those equations and
> generated-type references. See the contract's
> [incompatible-retained-epochs section](../../specs/backends/haskell.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-haskell).

## The build target

A package emits to Haskell by declaring a `haskell` target in
`<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target haskell {
    out "out/haskell/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build` or `kio build haskell` writes a self-contained Haskell
module under the package's namespace for the host to `import`. For a package
`greeter`:

```text
out/haskell/
└── Greeter.hs   // host types/functions + structural aliases/patterns + handle + wrappers
```

The package module is named after the package's **namespace** — the Kio
package name title-word-cased (`greeter` → `Greeter`, `my_pkg` → `MyPkg`), overridable with the
`namespace` target key. GHC maps a dotted module name such as
`Com.Acme.Greeter` to `Com/Acme/Greeter.hs`. The branded surface derives from
the namespace's exact final segment: the handle `Greeter`, the host contract
`GreeterHost`, the marker class `GreeterHostTypes`, and the factory
`createGreeter`. Private runtime support is declared in the same file without
being exported. Every declaration is scoped beneath the package's distinct
namespace module, and private runtime names additionally derive from the full
namespace, so two kio packages load into one Haskell program without
colliding.

An explicit namespace is retained exactly, without a second casing pass.
Affixed source names and defaults colliding with `Main` or `Prelude` use an
exact `KioPkg_...` namespace (`main` → `KioPkg_Main`); see
[the namespace rules](../../specs/backends/haskell.md#output-layout).

## How Haskell hosts the package

Haskell's own type system expresses higher-kinded and rank-N
polymorphism, so Kio's kind-`*→*` carriers map straight onto Haskell
type constructors (`F(A)` → `f a`) — there is nothing to wrap or unwrap
at the boundary. This matters in practice in two ways:

- A higher-kinded value (a `Functor` / `Monad` dictionary's `F(A)`)
  rides Haskell's own type-constructor application; the host pays no
  representation cost for higher-kinded types.
- A polymorphic position you *do* see at the boundary (a rank-N or
  existential payload) crosses at its native `forall` / existential
  type — you receive a real polymorphic Haskell value, not an opaque
  handle you must cast. Applying a type to a first-class polymorphic value
  returns the next stage through the package monad, so bind that result before
  calling the resulting value function.

The package is polymorphic in two host choices:

- A marker type `h` selects the exact Haskell representation of every
  Kio `host type` through the package's `<Handle>HostTypes h` class.
- A monad `m` carries effects. Use `IO` for printing and input, or a pure
  test monad such as `Identity`, `State`, or `Writer` when appropriate.

The marker class contains only associated types. Host functions remain
an ordinary **value** record of `m`-returning functions. Kio dictionaries
remain Kio values; they are not translated into Haskell typeclasses.

Every Kio `host type` gets its own associated family, identified by its
declaring module as well as its leaf name. That keeps two same-named
declarations in different modules separately addressable; their equations
may still select the same Haskell type or type constructor. Parameterized
roleless host types become associated families with one Haskell `Type`
argument per Kio parameter. A Kio role does not pick the Haskell
representation: `role(str)` may be `[Char]`, `Data.Text.Text`, or a domain
type selected by the host. The role only admits Kio's string syntax; ordinary
`IsString` constraints check a selected type when the package actually
contains such a literal. A Boolean literal or a value used by `if` requires
its selected type to be `Bool`.

### Strict evaluation in a lazy host

Kio is strictly evaluated; Haskell is lazy. The backend bridges the two
so you do not have to think about it: host effects are sequenced through
the monad (`>>=`), so the order Kio specifies is the order your `IO`
actions run, by construction; and bound values are forced where a lazy
thunk would change observable behavior. From the host's side, a `print`
followed by another `print` inside one Kio function fires in that order.

Generic calls preserve the same order without adding runtime type arguments to
the host API. Exported calls and callbacks still take only their documented
value arguments; generated adapters perform any required sequencing before
the next host-visible value call.

Memory is **GC** — the package body threads no ownership annotations and
the host needs none.

## Loading the package

A Haskell host brings the package online in five steps. The complete
program in § Worked example shows them together.

### 1. Import the module

Import the package module qualified — `import qualified Greeter` — so
its package-specific names remain clear. Where you define the marker
instance, also import `GreeterHostTypes(..)` unqualified; GHC requires an
associated-family name to be unqualified inside its instance equation.

If the configured namespace has the same spelling as a standard module, the
host enables `PackageImports` and gives the two modules distinct qualifiers.
For a package namespace `Data.Text`, use `import qualified Data.Text as Pkg`
for the artifact and `import qualified "text" Data.Text as Text` for the
standard module. The marker-instance module can use
`import Data.Text as PkgTypes (TextHostTypes(..))`: the class remains available
unqualified for its associated-family equations, while the artifact's original
module qualifier stays out of scope.

If the host constructs or matches a structural value, enable
`PatternSynonyms`. A structural value containing a polymorphic field also
needs `ImpredicativeTypes` (and `RankNTypes` to write that field's type).
Using the generated patterns does not require `TypeApplications` or
`AllowAmbiguousTypes`. Host code that explicitly selects a first-class
polymorphic value's type with visible application such as `value @Bool`
does require `TypeApplications`.

### 2. Bind every host type

Enable `TypeFamilies`, declare a marker such as `data AppTypes`, and
define `instance Greeter.GreeterHostTypes AppTypes`. Give every live
associated family the Haskell type this host uses. The generated family
normally reads `HostType__<module path>__<leaf>`, with word casing per component
(`data_api.Native_token` → `HostType__dataApi__NativeToken`), when every source component
contains no `__` and neither starts nor ends with `_`. Other source names use
a longer exact fallback. A configured namespace that overlaps the primary also
selects that fallback; if the fallback itself overlaps a fixed facade type, it
gets a namespace-qualified escape. Copy an exceptional spelling rather than
constructing it. A declaration
`host type Array[T]` expects a constructor of kind `Type -> Type`, not one
equation per element type.

GHC rejects the marker instance if any live equation is absent, even when
the package never otherwise uses that host type. The error names the exact
Kio `module/type`, so the missing choice can be copied from the emitted
class. Representable removed compatibility members are different: an older host may keep
its equation, while a new host may omit it and inherit the generated private
default. The generated family carries GHC's native
`{-# DEPRECATED type … #-}` marker, so an older equation remains source-visible
as deprecated. If that exact Kio host type is live again, the family is an
ordinary required member and is not deprecated. The retained family is
type-level compatibility only: it adds no host-record field, package
capability, or execution path.

If sealed history contains incompatible declarations at one exact nominal
identity, no generated Haskell type can preserve both frozen signatures. The
generator omits the affected retained equations and type closure. Existing
host source must delete or rewrite those equations and direct generated-type
references; current instances remain live-only. See the caveat banner above.

The host makes these choices, including for role-bearing declarations.
GHC then checks any constraints the Kio source induces. A string literal
requires `IsString` for the selected string type; an integer literal
requires `Num`; and a Boolean literal or selected value used by `if` must
be `Bool`. A role does not silently replace the selected type with a
backend lookup-table entry.

### 3. Build the host-function record

Each `host fn` is one field on `GreeterHost AppTypes m`. Generated fields use
the stable `host__<module path>__<leaf>` form. For example, `host fn print` in
module `greeter` is `host__greeter__print`. A component containing `__`, or
starting or ending with `_`, uses the exact fallback shown in the emitted
record.
Copy the exact field from the emitted `GreeterHost` declaration (or use editor
completion) instead of constructing that identity by hand. Each field is a
function returning `m`.

GHC checks the record against the associated types selected by
`AppTypes`. A wrong argument, result, or effect type is a host compile
error. The marker class carries no function values, and the record
carries no type bindings.

### 4. Create the package

`createGreeter stubHost` returns `Greeter AppTypes m`. Its emitted context
contains only `GreeterHostTypes AppTypes` and `Monad m`. There is no runtime
loading failure: an incompatible choice is a compile error.

### 5. Invoke exported items

A `pub fn` named `fn` in module `a/b` is reached through the top-level
wrapper
`export__a__b__fn`, which
takes the `Greeter AppTypes m` handle and the function's value parameters,
returning `m <result>`. A newtype member includes both the type and member
components, such as `export__api__Pair__makePair` for member `make_pair`.
A component containing
`__`, or starting or ending with `_`, uses the exact fallback shown in the
emitted module.

Readable source components preserve initial case and outer underscores while
joining internal words (`do_work` → `doWork`). Exact fallback names retain
their complete raw byte identity; only their cosmetic readable suffixes use
word casing. Fixed `__` route boundaries and role/index components are not
source word separators.

The wrapper runs the function while threading effects through `m`. Its context
also contains any ordinary literal or conditional constraints used by that
function or by a top-level function value it transitively reaches; unrelated
package code contributes none.
Concrete host types remain the associated types chosen by `AppTypes`, and
structural values remain native Haskell shapes; the package's internal
value model is not part of this interface.

Kio's bottom type `!` appears as `Data.Void.Void`, both directly and inside
structural or newtype boundary shapes. A host function that accepts such a
value can eliminate it with `Data.Void.absurd`. A host function returning `!`
has a result such as `IO Data.Void.Void` and must diverge, for example by
terminating the process; it cannot manufacture a normal return value.

## Structural values at the boundary

A structural Kio type crosses as ordinary Haskell pairs or `Either`, named by
a stable alias for the particular host-function or exported-function slot:

- A product `(A & B & C)` has the underlying shape `(a, (b, c))`.
- A sum `(A | B | C)` has the underlying shape
  `Either a (Either b c)`.
- Unit and singleton folds stay natural: an empty product is `()`, an empty
  sum is `Data.Void.Void`, and a one-slot fold is that slot itself.

When a product is the value domain of a public callable stage, call the
Haskell function with the product's canonical slots as separate curried
arguments. The same rule applies to a product-domain callback. A direct Unit
domain is called with no term argument; an existing generic slot instantiated
with Unit is still passed as `()`. Generated adapters rebuild the package
body's original tuple/domain representation. Product patterns remain the
right interface for a product that crosses as one value, such as a returned
product or a product nested inside another boundary shape.

The package exports `<Handle>Product` and `<Handle>Sum` type families that
perform those folds. More commonly, use the generated `Env_H…` alias for a
host-function slot or `Exp_H…` alias for an exported-function slot. The alias
keeps exact host types, functions, and generic parameters in their original
positions, including repeated parameters. Substituting a product or sum for a
generic parameter therefore gives the same Haskell type as writing the
expanded Kio type directly.

Those are the normal spellings. If an explicitly chosen target namespace
would make a structural family, alias, or pattern reuse another generated
Haskell name, the package gives the structural declaration a stable
`KioStructT_H…`, `KioStructC_H…`, or `kioStructV_H…` spelling instead. The
choice is determined by the target namespace and the declaration's identity,
so unrelated Kio declarations cannot rename it. As with the exact host-family
and public-item names, copy an escaped spelling from the generated module
rather than constructing it by hand.

You do not need to write the nested pairs and `Either`s. Each product alias
exports a flat record-style `EnvP_H…` or `ExpP_H…` pattern, with separately
exported `envSel_H…` or `expSel_H…` selectors. A slot that resolves to any
newtype uses its 3-step-fallback key; a slot that does not resolve to a newtype
uses a positional key. Each sum alias exports one `EnvS_H…` or `ExpS_H…`
pattern per arm, using the same key rule. Copy these names from the emitted
module. The patterns are bidirectional, so the same spelling works when
constructing or matching a value, and their `COMPLETE` declarations make
exhaustiveness checking understand the whole shape.

## Host types and literals

Every Kio `host type` crosses as the exact associated family selected by
`h`. There is no fixed role-to-Haskell table. For example, a host may
choose any of these representations if its package's use sites impose
the corresponding Haskell constraints:

- `host type Str role(str);` may be `[Char]`, `Data.Text.Text`, or a
  custom `IsString` type.
- `host type Count role(i32);` may be `Integer`, `Data.Int.Int32`, or a
  custom `Num` type.
- `host type Flag role(bool);` may be any type when it is used only at the
  host boundary, but a Kio Boolean literal or conditional requires it to
  be `Bool`.
- `host type Token;` may be any host-defined type; it needs no role.

An annotated Kio literal is emitted as an ordinary overloaded Haskell
literal at the selected type. Boolean literals become `True` / `False`;
a Kio conditional is emitted as an ordinary Haskell conditional. Both
therefore require the selected type in that position to be `Bool`. These
are use-site requirements, not alternative representation choices made by
the backend. `()` is native Haskell `()`.

## Newtypes and function values

Every public newtype in a bridged module has its own host-facing surface. Even when it exports no
member wrappers, it has a declaration-specific abstract type. With only a
public constructor, the host can create values but cannot inspect them; with
only a public projector, it can inspect values received from the package but
cannot create them. The raw Haskell constructor and storage stay hidden in all
three cases.

With both members public, the established representation remains in place. A
nullary, non-recursive, non-existential newtype is transparent, so its wrappers
use the payload type directly. A parametric or recursive newtype instead has
one deterministic, abstract `KioCarrier_H…` Haskell type constructor. That
remains the same type constructor whether it is partially or fully applied.
The carrier scheme includes dictionary-shaped and plain function-payload
newtypes.

An **existential newtype** crosses as a native Haskell existential carrier.
Its constructor accepts the exact payload and seals the hidden binders. Its
projector accepts a rank-N continuation. With one hidden type, a one-slot
payload has the form `forall hidden. m (payload -> m r)`; canonical Unit has
no runtime argument and uses `forall hidden. m (m r)`. Bind the result of the
visible type application before using the payload stage. More hidden types
repeat that `forall hidden. m (...)` shape. This lets the host inspect the
payload without letting a hidden type escape. A product payload supplies its
canonical slots as separate continuation arguments, just like any other
function-value domain. The carrier has a deterministic
`KioExistential_H…` name in the emitted package module and is exported
abstractly; hosts construct and inspect it only through the exported Kio
member wrappers. When an existential carrier appears inside a product or
sum, it remains an abstract leaf of the pair / `Either` fold. Universal
carrier arguments keep their direct native representation, so the carrier
itself crosses by identity.

Each public wrapper keeps the generic types declared by the package. Carrier
identity includes the exact declaring module and type, so same-named
newtypes in different modules remain distinct. An enclosing boundary shape
treats an abstract carrier as one value and never exposes its hidden payload.

A **function value** crosses as a native curried Haskell function `a -> … ->
m ret`; its product domain is exposed as separate canonical arguments and a
direct Unit domain is nullary. Its compound legs are converted across the
boundary, but the function itself is a real Haskell function you can call or
store. Generic host functions quantify their type parameters with native
Haskell `forall`, including higher-kinded parameters; this adds no term-level
type-argument slots to a host call. Generated adapters preserve the package's
evaluation order before the next visible value call. When a function value
itself starts with `[A]`, its Haskell form starts with
`forall a. m (...)`; use `value @Concrete >>= ...` to obtain and continue with
the next stage.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target haskell {
    out "out/haskell/"
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

host type Str role(str);
host type Output;
host fn stdout() -> Output;
host fn print(output: Output, s: Str) -> .;
```

`greeter/main.kio`:

<!--kio {file}
package greeter;

build {
  cache "out/.kio-cache/";

  target haskell {
    out "out/haskell/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_haskell_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(Str, print, stdout);

__SNIPPET__
-->

```kio {@greeter_haskell_main placeholder={"module greeter/main;":"","import greeter(Str, print, stdout);":""}}
module greeter/main;

import greeter(Str, print, stdout);

pub fn main() -> . { print(stdout(), "hello\n"(Str)) }
```

`kio build haskell` writes `out/haskell/Greeter.hs`.
The host binds the role-bearing `Str` to `[Char]` and the role-free
`Output` to a host-defined ADT, then supplies the two functions:

```haskell
{-# LANGUAGE TypeFamilies #-}

import Greeter (GreeterHostTypes(..))
import qualified Greeter

data Output = Stdout
data AppTypes

instance Greeter.GreeterHostTypes AppTypes where
  type HostType__greeter__Str AppTypes = String
  type HostType__greeter__Output AppTypes = Output

stubHost :: Greeter.GreeterHost AppTypes IO
stubHost = Greeter.GreeterHost
  { Greeter.host__greeter__stdout = pure Stdout
  , Greeter.host__greeter__print = \output s ->
      case output of
        Stdout -> putStr s
  }

main :: IO ()
main = do
  let pkg = Greeter.createGreeter stubHost
  Greeter.export__greeter__main__main pkg
```

This prints `hello` and exits 0.

## Where this leads

- [`specs/backends/haskell.md`](../../specs/backends/haskell.md) — the full
  backend contract: output layout, boundary semantics, FFI surface, item
  naming, and the carve-out catalogue.
- [`specs/backends/README.md`](../../specs/backends/README.md) — cross-cutting
  properties shared by every backend, including where Haskell sits in the
  language family taxonomy.
- [Getting started with the Kio tooling](../tutorials/tooling.md) — build and
  check a package before loading it in a host.
- [`specs/cli.md`](../../specs/cli.md) — the exact `kio build` command
  contract.

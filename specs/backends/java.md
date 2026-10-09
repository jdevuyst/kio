# Java backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `erased-static` (see [`README.md` § Language families](README.md#language-families)).

> **Known host-source compatibility caveat.** Removing a nullary bridged
> `host type` removes its package-root generic argument and changes retained
> signatures that depended on the selected Java type to use a deprecated
> declaration-owned carrier. Existing host source must drop that generic
> selection and delete or rewrite dependent method and adapter overrides. Java
> has no optional or default type argument that can preserve those spellings
> without imposing a current-host selection. See
> [§ Host-type removal cannot preserve generic arity](#host-type-removal-cannot-preserve-generic-arity).
> When retained signatures require incompatible epochs of one exact nominal
> declaration, Java omits the affected methods and type closure; existing host
> source must delete or rewrite their overrides and generated-type references.
> See
> [§ Incompatible retained declaration epochs are not source-stable on Java](#incompatible-retained-declaration-epochs-are-not-source-stable-on-java).

The `"java"` build target ([`specs/package.md` § Build target files](../package.md#build-target-files))
emits a typed Java facade over an interpreter body. This page describes
the surface the host calls.

**Family divergence — body strategy.** The `erased-static` family's
members generate per-module host source; the Java backend instead
serializes the routed package IR and evaluates it with an emitted
interpreter (`KioRuntime.java`), erasing the body to `Object` — the
host's universal object — exactly as the family erases to its dynamic
carrier. The divergence is emitter-internal: the observable contract —
a nominal typed FFI skin over an erased body — is the family's. The
emitter code carries the mutual cite
(`kio-rs/src/backends/java/mod.rs`).

## Language version

Emitted code targets **Java 21+** (sealed interfaces, records, pattern
switches). The generated source uses only the Java standard library.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target java {
    out "out/java/";
  }
}
```

`kio build java` writes the package under its **namespace directory**
`<out>/<ns dirs>/`, where the namespace defaults to the package name and
the optional `namespace "<value>"` target key overrides it with a dotted
chain of Java identifiers, none a reserved word
([`specs/package.md` § Per-target keys](../package.md#per-target-keys));
a default that collides with a Java reserved word or whose handle would
land on an emitted support class (`Shapes`, `KioRuntime`) uses the exact
`__kio_pkg_<escaped-source>` namespace (`_` → `_u`). For example,
`shapes` uses namespace `__kio_pkg_shapes` and handle `KioPkg_Shapes`.
An explicit value whose final segment derives a reserved support handle,
or no handle at all, is rejected.
The **handle name** `<Handle>` derives from the effective namespace's final
segment. Ordinary source-shaped names use title word casing (`greeter` →
`Greeter`, `com.acme.csv_tools` → `CsvTools`). Affixed source names and
reserved defaults use the `KioPkg_...` brand; other explicit final segments
use `KioNs_<hex-UTF-8>`, following [the shared brand encoding](README.md#branded-naming).
The Java package path retains the effective namespace spelling.
Four files:

- `<Handle>.java` — the package handle: the typed facade the host
  instantiates and calls (`public final class <Handle>`).
- `<Handle>Host.java` — the host contract: `public interface
  <Handle>Host`, one method per `host fn`.
- `Shapes.java` — the boundary shape declarations: one nominal Java
  type per distinct FFI shape (`public final class Shapes`, nested
  types).
- `KioRuntime.java` — the interpreter body (package-private). Internal
  to the package's emit, not user-modifiable, and not part of the host
  contract.

Every file carries `package <ns>;`. Two packages with distinct
namespaces coexist in one host program; their classes are distinct
Java types.

## Loading protocol

A host that wants to call into the package performs three steps in order.

### 1. Compile the generated source

```sh
javac out/java/greeter/*.java
```

Compilation performs no package I/O and calls no host functions; the
package comes online when the host invokes the factory.

### 2. Implement the host contract

`<Handle>Host` declares one method per `host fn` and one exact adapter pair
per `host type` the bridged modules declare, at typed Java signatures (see
[§ Host record contract](#host-record-contract)):

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

Java's ordinary single-abstract-method rule applies to the complete generated
interface. Each live host-type binding contributes two adapter methods, so an
interface that also has one host fn is not a functional interface.

### 3. Instantiate and invoke

The handle's static factory takes the host and returns the package
handle; exports are reached through the typed namespace fields:

```java
import greeter.Greeter;

Greeter<String> pkg = Greeter.create(new Stdout());
pkg.greeter.KioModule_main.main();
```

A package that has neither host fns nor live or retained host-type bindings
additionally has a host-less `<Handle>.create()` overload.

Construction validates the host record and evaluates no package code;
each export runs when called. Instantiating twice yields two
independent instances ([`README.md` § Package
isolation](README.md#4-package-isolation)).

## Package API

The factory returns the package handle: `public final class <Handle>`
with one `public final` field per top-level export namespace. A root module
uses word casing followed by Java identifier escaping. Every nested module uses
`KioModule_<exact-component>`, and every public newtype handle uses
`KioType_<exact-component>`; word casing precedes escaping remaining `_`
as `_u`. An export `main`
from module `greeter/main` is therefore reached as
`pkg.greeter.KioModule_main.main()`. Namespace fields are typed by generated
namespace classes; their class names are internal structure, the fields are
the contract.

- **Exported fns** are typed methods on their namespace value: one
  parameter per declared value parameter (across all value groups, in
  order), the declared return type, both at the boundary Java types of
  [§ FFI surface](#ffi-surface). A unit return is `void`.
- **Exported newtypes** always contribute a per-type handle
  selected as `KioType_<exact-component>` under its module namespace, even
  when that handle has no methods. It carries
  exactly the constructor and projector whose declarations are `pub`,
  as typed methods (`pkg.types.KioType_Pair.mkPair(payload)`). The boundary
  value depends on that public member set:

  | Public members | Java boundary value |
  | --- | --- |
  | neither | a declaration-owned opaque class, such as `Shapes.KioModule_api.Token` |
  | constructor only | that class, constructible only through the method |
  | projector only | that class, inspectable only through the method |
  | both, no existential | a declaration-specific class with a typed payload constructor and `value()` accessor |
  | projector with an existential | the opaque class plus a CPS-shaped projector |

  This table describes nullary declarations. A parameterized declaration uses
  the exact nested `Apply1` index described below with the same member visibility.

  The opaque and one-member classes keep their storage and raw
  construction package-internal, outside the public host facade. Each
  selected member retains its exact declared Kio scheme after the Java
  boundary mapping. An existential projector is CPS-shaped: its receiver method returns
  the declared type stages and continuation stage, whose invocation produces
  the continuation's result.
- A both-member recursive newtype keeps its nominal class and refers to that
  same carrier from the prepared payload type. An opaque or one-member carrier
  is a finite nominal leaf, so its hidden payload is not traversed while
  deriving an enclosing boundary shape.

Carrier identity is the exact declaring module plus newtype name, realized
under the module holders in `Shapes` (see [§ Item naming](#item-naming)). The handle, host interface,
namespace nodes, and declaration-owned newtype carriers and markers carry
the package-root host-type parameters in declaration-keyed order. Canonical
products, sums, and functions carry only their constituent type indices;
first-class type-stage interfaces carry their independent captured indices.
Same-leaf declarations in different modules therefore remain distinct,
and adding an unrelated declaration cannot rename or rebind an existing
carrier.

### Doc comments

The [shared documentation contract](README.md#doc-comments-on-the-emitted-surface)
uses Javadoc on exported methods, host-interface methods, and minted
host-type and newtype carriers and constructor markers. Nullary host types
represented only by generic parameters have no separate declaration slot.
Kio documentation markup is rendered as escaped Javadoc HTML; authored tag
syntax and unresolved references remain inert text.

## Host record contract

`<Handle>Host` declares one method per `host fn`. A declaring module path
without source `_` keeps the readable rung-2 mangling
`<module path, / → _>__<leaf>` (`greeter__print`,
`testapi_arith__addI32`). A path containing `_` instead uses
`KioItem_<exact-path>__<leaf>`, where word casing precedes escaping
remaining `_` as `_u` and `/` as `_s`. Both forms word-case the callable
leaf (`do_work` → `doWork`); `data_api.do_work` therefore becomes
`KioItem_dataApi__doWork`. The mapping is module-qualified and injective
([`README.md` § 8](README.md#8-host-trait-descriptor)). Parameters and
returns use the boundary Java types of [§ FFI surface](#ffi-surface); a unit
return is `void`.

Every nullary `host type` contributes an independent package-root Java type
parameter named from its exact qualified declaration. `<Handle>`,
`<Handle>Host`, their factories and namespaces, all public newtype carriers,
and every boundary occurrence use that same parameter. `<Handle>Host` also
declares the exact-QTN adapter pair
`KioHostBinding_<declaration>_fromBody` / `_toBody`; the host implements these
to translate between its selected reference type and the canonical role value
used by literal construction and condition inspection. Ordinary calls transport
that selected reference unchanged, including through an abstract type or an
opaque host container. Two declarations with the same role remain two type
parameters and two adapter pairs even when the host selects the same Java type
for both. This is Java's exact-frame realization of the shared
[declaration-keyed conversion-adapter capability](README.md#declaration-keyed-conversion-adapters);
Go and Python use that capability's readable-primary naming strategy instead.
A parameterized host declaration uses the canonical nested `Shapes.Apply1`
index rooted at its exact declaration marker, such as `Shapes.KioModule_api.Box`, and
contributes a generic adapter pair. Its declaration-owned
`Shapes.KioModule_api.Box.Storage` helper supplies typed `fromNative(Object)`
and `toNative(...)` storage operations. These helpers apply only to that host
declaration, not to a newtype or an arbitrary application.

The binding lookup is exact qualified identity, never role, leaf spelling,
declaration order, or another declaration's selected Java type. Adding an
ordinary unrelated declaration therefore cannot change an existing binding.
Adding or removing a declaration in the bridged host/export contract changes
that contract surface and is governed by package signature versioning.

Construction fails with an `IllegalStateException` naming the first
missing host item if the interpreter's host-record validation finds a
gap — unreachable through the typed factory, which accepts only a
complete `<Handle>Host` implementation.

## Deprecated host items

A removed `host fn` whose frozen signature has no removed host-type dependency
or incompatible nominal epoch is re-emitted as a `@Deprecated` `default` method
with a throwing body — the Java analogue of the Rust backend's
`#[deprecated]` diverging default ([`README.md` § Deprecated host
items](README.md#deprecated-host-items)) — so a host still
`@Override`-ing the removed member keeps compiling across the removal.
The package never calls it. A removed `host type` no longer occupies a
package-root generic parameter; a deprecated declaration-owned `Shapes`
carrier and deprecated throwing adapter defaults keep frozen signatures
nameable without requiring a current host to select or implement the removed
binding. A retained nullary declaration's carrier is an inert interface;
parameterized declarations keep their marker and storage helper. Java cannot
default a removed generic parameter, so old source that
explicitly supplied that history-only type argument must drop it. An old
method or adapter override whose signature used that selected type must also be
deleted or rewritten against the declaration-owned carrier.

Every emitted public `Shapes` declaration, constructor, accessor, callable
helper, and type-constructor witness reached only by history is likewise
`@Deprecated`.
If a live declaration reaches the same support identity, the shared declaration
is live and nondeprecated.

### Host-type removal cannot preserve generic arity

Java has no optional or default type argument. Keeping a removed nullary host
type in the package-root generic parameter list would force every current host
to select a history-only binding, so the backend removes that parameter. An
existing host must drop the old argument from generated host-interface,
handle, factory, namespace, callback, or carrier spellings that explicitly
selected it. A retained method or adapter signature instead names the
deprecated declaration-owned `Shapes` carrier. An old `@Override` written
against the previously selected Java type therefore no longer overrides that
signature and must be deleted or rewritten against the carrier. The retained
methods have throwing defaults, so neither type selection nor adapter
implementation is a current-host obligation.

The degradation site is the `live_root_type_parameters` filter in
`JavaShapes::new` in [`skin.rs`](../../kio-rs/src/backends/java/skin.rs), whose
comment cites this subsection in turn.

### Incompatible retained declaration epochs are not source-stable on Java

One generated Java nominal identity cannot describe two incompatible exact
declaration epochs. Under the shared
[`README.md` rule](README.md#incompatible-retained-declaration-epochs), Java
omits every retained method, adapter, `Shapes` declaration, and transitive
support item that depends on the conflict. An old `@Override` then no longer
overrides a generated method, or its generated parameter type no longer
exists, so affected host source must delete or rewrite the override and any
direct generated-type reference. Current hosts acquire no history obligation.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs); its
comment cites the shared rule's explicit backend fan-out to this subsection.

This is a host-callable compatibility surface, not live package execution.
Under [`README.md` § Facade topology and execution
provenance](README.md#facade-topology-and-execution-provenance), only current
host methods enter the interpreter's host adapter; a deprecated default stays
on the host interface but contributes no adapter entry or path from package
code. The same rule excludes retained host-type adapters from live conversion
paths.

## Boundary semantics

Calls in both directions are synchronous and re-entrant
([`README.md` § 1](README.md#1-synchronous-single-threaded-re-entrant-calling-convention)).
Exceptions propagate unchanged in both directions
([`README.md` § 5](README.md#5-exception-propagation)): a host method
that throws unwinds through the package to the originating call site,
and a package runtime guard surfaces as an unchecked Java exception.
Arguments are evaluated call-by-value in declaration order
([`README.md` § 9](README.md#9-call-by-value-evaluation-order)).

## FFI surface

Host values follow the shared
[declared-type responsibility](README.md#11-host-values-satisfy-their-declared-types).

### Host-type bindings

The public type of each nullary `host type` is its exact package-root type
parameter, selected by the host. A role determines only the canonical private
body type accepted and returned by that declaration's adapters:

| role | private adapter body type |
| --- | --- |
| `str` | `String` |
| `bool` | `Boolean` |
| `i8` / `i16` / `i32` / `i64` | `Byte` / `Short` / `Integer` / `Long` |
| `u8` / `u16` / `u32` | `Short` / `Integer` / `Long` |
| `u64` / `i128` / `u128` | `java.math.BigInteger` |
| `f32` / `f64` | `Float` / `Double` |

A roleless declaration uses `Object` on the private adapter side. None of
these body types collapses the declaration-keyed public types: the host may
select one Java type for several declarations or different Java types for
declarations sharing a role.

A lexical literal can select an exact host-type declaration only when its
lexical role matches that declaration's role. An annotation or expected type
chooses the exact declaration; it does not change the literal's role. A
roleless host type therefore has no matching lexical literal. After this
compile-time check, Java constructs the value through that declaration's
existing role-shaped private body type and exact adapter pair.

### Structural and nominal types

These values follow the shared
[unchanged-data rule](README.md#10-shared-data-stays-unchanged). Record
shells do not remove that obligation for nested referenced Kio data; opaque
host state retains its declared operations.

Compound values use these canonical types in `Shapes.java`, in package bodies
as well as at the public boundary:

- A **product** `A & B` is `Shapes.Product<A, B>`, a record with `left()`
  and `right()` accessors. A longer right-associated product uses nested
  binary products; an explicitly left-nested product retains that nesting.
- A **sum** `A | B` is `Shapes.Sum<A, B>`, a sealed interface implemented by
  `Shapes.Left<A, B>` and `Shapes.Right<A, B>`. Both variants retain both
  type indices and expose their selected payload through `value()`. Longer
  sums retain the same binary nesting as their Kio type.
- A **function value** `A -> R` is `Shapes.Fn1<A, R>`, whose `apply`
  method takes exactly one domain value. Unit is explicit at this stage:
  `. -> .` is `Fn1<Shapes.Unit, Shapes.Unit>`.
- The canonical **unit** is the empty record `Shapes.Unit`. A named
  declaration returning Unit uses Java `void`; that method view does not
  alter first-class function values.
- A **first-class forall** has one generic `advance()` method for exactly
  one binder. Its result is the precise remaining callable or value type.
  Consecutive binders therefore require consecutive advances. Java lambdas
  cannot implement generic abstract methods; a host supplies an anonymous
  class or another implementation of the generated interface. A type-stage
  effect runs when that advance occurs, not when a later function is called.
  Named declaration-head binders remain Java method type parameters.
- A nullary **newtype** uses its exact declaration-owned class, such as
  `Shapes.KioModule_api.Token`. With both members public and no
  existential it has a typed public payload constructor and `value()`
  accessor. Opaque, one-member and existential classes hide their payload
  and raw constructor; only the selected package methods expose their
  authorized construction or inspection. Parameterized newtypes use the
  canonical application index below with the same privacy.
- **Bottom** is `Void`. A **type alias** uses its expansion without adding a
  carrier or changing its surrounding source-parameter group.

These types satisfy the shared
[generic substitution and opaque-constructor law](README.md#generic-substitution-and-opaque-constructors).
A concrete occurrence and its generic substitution use the same native index
and value realization; an opaque host container does not need to expose,
traverse or convert its payload.

Only a named method whose source groups exceed the JVM reference-parameter
limit uses a flat prepared argument shell. Its typed accessors and builder
retain the prepared semantic keys; it is an argument-container convention,
not a second representation of a first-class product. Such shell names use
the shared facade codec, with the existing bounded Java mint and collision
check; wider all-positional shells use the exact
`KioFacade_V1_Product_Positional_K<arity>` spelling.

Callable parameters follow the shared [source-parameter grouping
contract](README.md#function-type-ffi-canonicalization): each declared value parameter is
one Java parameter, with leading declaration groups compacted into the method.
Thus `first(x: A, y: B)` takes two arguments while `second(pair: A & B)` takes
one product value, even though their Kio types are equal. Type aliases expand
inside their existing parameter. A returned function stays a returned callable;
its domain is not appended to the declaration's method parameters. A declared
Unit parameter is omitted in this named-method view; a generic parameter later
instantiated with Unit still takes a `Shapes.Unit` value. First-class function
values always take their one domain value, including Unit.

An exported or host instance method through 254 native reference parameters
exposes those groups directly. A wider method takes the exact prepared
whole-head product shell, built with its typed builder. The Java wrapper uses
the paired execution layout to bridge semantic slots to the private
interpreter arguments independently of the public grouping. This cutoff
changes only the JVM realization: semantic keys, record fields, sum arms, and
retained source grouping still come from the same prepared plan.

## Higher-kinded types

The public index of every application is unary:
`F(A)` is `Shapes.Apply1<F, A>`, and `F(A, B)` is
`Shapes.Apply1<Shapes.Apply1<F, A>, B>`. A partial and a saturated nominal
application use this same rule. The exact constructor marker is the
declaration-owned interface, such as `Shapes.KioModule_api.Box`.
Newtype markers carry the package-root host selections used by their
declaration; host-type markers need no such arguments.

`Apply1` exposes no general raw constructor or payload accessor. Ordinary
newtype members provide precisely the operations their visibility permits.
A host declaration's own nested `Storage` helper can store and
retrieve host-owned storage at its exact application index; it cannot rebrand
another declaration or project a newtype. Passing a value through abstract
`T` or `F(A)` preserves that value without structural traversal.

First-class polymorphic values retain one `advance()` per binder even under
an opaque constructor. Generated interfaces factor the binder-dependent
shape from independent captured type arguments, so specializing a captured
type does not change the interface's nominal identity. The private erased
body stores instances of these same interfaces. It does not substitute a
different callable representation when constructing or projecting a newtype.
No public host call receives runtime type arguments or representation
witnesses. See the shared
[polymorphic-newtype payload contract](README.md#polymorphic-newtype-payloads).

## Item naming

At the package surface, a root module and ordinary function/member leaves use
[shared word casing](README.md#source-derived-public-names) (`do_work` → `doWork`,
`Native_token` → `NativeToken`). A Java keyword or otherwise restricted
identifier uses `KioItem_<exact-component>`. Exact components apply word casing
before escaping remaining `_` as `_u`; exact paths also escape `/` as `_s`.
Nested modules always use `KioModule_<exact-component>` and public newtype
handles always use `KioType_<exact-component>`. Host-interface methods use
the readable/exact mangling of [§ Host record contract](#host-record-contract).
The root module `java` also uses `KioModule_java`: its field would otherwise
capture the package qualifier used by generated support references. Runtime
module lookup retains the source identity. Ordinary functions named `java`
remain `java()`; a method name does not capture that qualifier.
Structural shell and nominal carrier names follow [§ FFI surface](#ffi-surface).
Host-binding and member components use the same word casing and exact escaping.
Class-bearing nominal and facade type names instead preserve source word
boundaries as described below. Presentation does not change raw declaration
or structural identity.
Declaration-owned types use module-structured paths under `Shapes`:
`api/types.Pick_first` becomes `Shapes.KioModule_api.KioModule_types.Pick_first`.
Module holders use `KioModule_<raw-exact-component>`; type leaves retain their
source spelling. Raw exact components escape `_` as `_u` without word casing.
These class-bearing names preserve source word boundaries under case-insensitive
filename comparison, as required by [the shared naming contract](README.md#source-derived-public-names).
Each holder is a noninstantiable static class. A nullary newtype owns its
existing carrier class; a parameterized host type or newtype owns its existing
constructor-marker interface. A retained nullary host type owns an inert
interface; live nullary host types remain package-root parameters.

When a module holder repeats an enclosing name under ASCII case folding, it
uses `KioModule_D<depth>_<raw-exact-component>`, with zero-based source depth.
A type leaf equal to an enclosing type name (including `Shapes`), or beginning
with `KioModule_`, `KioType_` or `KioNominal_`, under the same comparison uses
`KioType_<raw-exact-component>`. These fixed reserved classes are disjoint
from ordinary source-derived names. A parameterized
host type owns a nested `Storage` helper; when that helper would repeat an
enclosing type name under ASCII case folding, its name is `KioStorage`.

A nominal path's complete classfile basename, including `Shapes$`, every `$` separator,
the longest fixed `$KioStorage` helper suffix, and `.class`, is bounded to
240 ASCII bytes. If a nominal path exceeds that budget, its declaration
instead lives directly under `Shapes` as `KioNominal_<role>_H<digest>`, with
role `Host` or `Newtype` and the 16-digit lowercase FNV-1a-64 digest of
`<role>:<source-module-path>/<source-type-name>`. A full-identity collision
is rejected, never assigned a sibling-dependent suffix. Only holders used
by selected nested paths are emitted. Placement and names depend on the
declaration's own identity and fixed reservations, never on its siblings.

Export namespace classes use `Ns_` followed by raw exact source components
with `M_` (module) and `T_` (type) roles, separated by `__`. Newtype-handle
classes use the same framing with `NtH_`. Each class identifier is at most
64 bytes: longer identities use `<prefix>_H<digest>`, with the 16-digit FNV-1a-64
digest of the complete prefixed raw-exact identity. Distinct full identities
with the same case-folded classfile name are rejected. This component bound
does not bound the separately configured outer package-handle brand.

A first-class `forall` interface uses a readable `KioForall_` shape name.
For example, `[A] A -> A` uses `KioForall_Type_Fn_T0_T0`, whose generic
`advance()` returns `Fn1<A, A>`. Independent captured subtrees are interface
type arguments, one per prepared occurrence. The name does not contain a
callable's source site or depend on other declarations in the package.

The suffix is an underscore-separated prefix encoding: `Type` or binary
`Arrow` encodes a binder kind; `Tn` is its zero-based lexical depth, with the
innermost binder at zero. `Fn`, `Pair`, and `Sum` each precede two children;
`Unit` and `Never` are leaves; `ApplyN` precedes its constructor and N type
arguments. The leading binder's kind comes first; a nested binder uses
`Forall`, its kind, then its result. When captures exist, `CapturesN` and
their N kinds precede the leading binder's kind. Thus `[A][B] A -> B` has
name `KioForall_Type_Forall_Type_Fn_T1_T0`. Captures retain occurrence order
and kinds even when substitution makes two of their selected types equal.

Through 64 UTF-8 bytes the reversible spelling is used directly; longer names use
`KioForall_H<hex>` with the existing complete-identity FNV-1a fallback and
collision check. The digest is computed from the full shared shape identity,
not from a truncated spelling; distinct full identities or declarations under
one native name are rejected rather than identified with each other.

## Worked example

`greeter.pkg.kio`:

```kio
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
```

`greeter.kio` declares the host capability; `greeter/main.kio` exports
`main`:

```kio
module greeter;

host type String role(str);

host fn print(p0: String) -> .;
```

```kio
module greeter/main;

import greeter(String, print);

pub fn main() { print("hello from kio\n") }
```

Java host:

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

See [`README.md`](README.md) for cross-cutting rules shared by every
backend.

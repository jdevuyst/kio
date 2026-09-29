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
  | neither | a declaration-specific opaque `Shapes.KioNewtype_<exact-declaration>` class |
  | constructor only | that class, constructible only through the method |
  | projector only | that class, inspectable only through the method |
  | both, no existential | a declaration-specific public record whose `value` has the prepared payload type |
  | projector with an existential | the opaque class plus a CPS-shaped projector |

  The opaque and one-member classes keep their storage and raw
  construction package-internal, outside the public host facade. Each
  selected member retains its exact declared Kio scheme after the Java
  boundary mapping. An existential projector is CPS-shaped: it takes the
  carrier and a continuation and returns the continuation's result.
- A both-member recursive newtype keeps its nominal record and refers to that
  same carrier from the prepared payload type. An opaque or one-member carrier
  is a finite nominal leaf, so its hidden payload is not traversed while
  deriving an enclosing boundary shape.

Carrier identity is the exact declaring module plus newtype name, encoded as
`KioNewtype_<exact-module>__<exact-name>`. The handle, host interface,
namespace nodes, structural shells, callbacks, and newtype carriers all carry
the same package-root host-type parameters in the same declaration-keyed
order. Same-leaf declarations in different modules therefore remain distinct,
and adding an unrelated declaration cannot rename or rebind an existing
carrier.

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
to translate between its selected reference type and the interpreter's private
canonical role value. Two declarations with the same role remain two type
parameters and two adapter pairs even when the host selects the same Java type
for both. This is Java's exact-frame realization of the shared
[declaration-keyed conversion-adapter capability](README.md#declaration-keyed-conversion-adapters);
Go and Python use that capability's readable-primary naming strategy instead.
A parameterized host declaration instead contributes a generated
declaration-owned `Shapes.KioHostType_<declaration><...>` carrier and generic
adapter pair, because Java has no higher-kinded type-parameter syntax.

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
binding. Java cannot default a removed generic parameter, so old source that
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

Compound boundary shapes are nominal Java types declared in
`Shapes.java`:

- A **product** uses one payload-generic shell selected by its ordered semantic
  keys. Through 254 reference components the shell is a `public record`; above
  the JVM constructor-slot limit it is a `public final class` with typed
  accessors and a typed `builder()`. Both forms stay flat at the public
  boundary. Member names follow the shared bare / qualified / positional key
  rule, and component order is right-spine order. Bare names use word casing;
  qualified names use `KioQualified_<exact-module>__<exact-type>`, word-casing
  each source component before exact escaping. Positional roles stay fixed.
- A **sum** is one payload-generic `public sealed interface` plus one generic
  `public record` variant per semantic arm. Its `KioMatch` method accepts one
  typed case per arm through 254 reference slots. Above that JVM limit it
  accepts one exact prepared product companion whose typed members are those
  case handlers; each variant invokes its matching member. A pattern switch
  over the sealed variants is also exhaustive.
- An **explicit newtype** uses the exact declaration-keyed
  `KioNewtype_<module>__<name>` carrier. A non-existential both-member carrier
  is a record with the prepared payload type. Opaque, one-member, and
  existential carriers are final classes with package-private `Object`
  storage; only the selected package-handle methods expose construction or
  inspection.
- A **function value** uses the reusable generic `Shapes.FnN<..., R>` or
  `Shapes.ProcN<...>` functional interface for its prepared flat value arity;
  hosts pass lambdas. A function stage wider than 254 reference slots takes
  its one exact prepared product shell instead, keeping the SAM within the JVM
  instance-method descriptor limit. Higher-kinded host applications use exact
  `Shapes.KioNewtypeMk_<declaration>` or
  `Shapes.KioHostTypeMk_<declaration>` markers and a typed
  `Shapes.ApplyN<F, ...>` carrier. Bound type stages do not become value
  parameters, and the wrapper adapts the private staged callable to the
  prepared public callable topology.
- The canonical **unit** is `Shapes.Unit` (an empty record) in slot
  positions and `void` at a top-level return.
- A callable-head type stage becomes a Java method type parameter. A
  first-class `forall` becomes an exact prepared functional interface whose
  one abstract `apply` method declares the quantified Java type parameters;
  consecutive leading binders share that method, and an immediately following
  function contributes its prepared flat parameters and result. Because a
  Java lambda cannot implement a generic abstract method, hosts construct such
  values with an anonymous class. Quantified types remain explicit throughout
  the public signature; only the package-private adapter instantiates those
  binders as `Object` when it crosses the erased interpreter body. Bottom is
  `Void`.
- A **type-alias-named slot** crosses as its expansion's shape: the alias
  is unfolded before the slot's Java type is planned, so it crosses exactly
  as its right-hand side would — a product / sum / newtype / function
  alias, or a nested alias use, all cross structurally — in both
  directions and for host-fn and exported-fn boundaries alike.

Product and sum shell names use the shared exact public facade codec. The
identity contains only structural kind plus the ordered semantic keys; payload
types are generic arguments at each use. Equal semantic shells therefore
share one declaration without equating their payloads, and a declaration's
name never depends on which other shapes were encountered. A readable encoded
name through 64 characters is used directly; above that fixed Java mint budget
the name is `KioFacade_V1_<kind>_H<hex>`, where `<hex>` is the 16-lowercase-
digit FNV-1a hash of the shell's complete exact encoded identity. Distinct
prepared identities are checked for a bounded-name collision and never share
a declaration. The anonymous binary shells retain the short names
`Shapes.Product` and `Shapes.Sum`. A wider all-positional shell uses
`KioFacade_V1_<kind>_Positional_K<arity>`; this remains exact because its
ordered positional sequence is determined completely by kind and arity, while
keeping the nested class filename within the host filesystem's component
limit.

Callable parameters are the prepared flat semantic slots. An exported or host
instance method through 254 reference slots exposes them directly; a wider
method takes the one exact prepared whole-head product shell, built with that
shell's typed builder. The Java wrapper uses the paired execution layout to
repack either form into the private right-nested interpreter representation
and to flatten results back out. The cutoff changes only the JVM realization:
semantic keys, record fields, sum arms, and the retained source contract still
come from the same prepared plan.

## Higher-kinded types

Type-erased, per the family strategy
([`README.md` § Higher-kinded types](README.md#higher-kinded-types)): a
kind-`*→*` carrier `F(A)` is an ordinary erased `Object` in the body,
exactly like a scalar. The public skin names an unsaturated nominal with the
exact category-specific `KioNewtypeMk_<declaration>` or
`KioHostTypeMk_<declaration>` marker and an application with
`ApplyN<F, ...>`; a fully saturated declaration uses its exact
`KioNewtype_<declaration><...>` or `KioHostType_<declaration><...>` carrier.
Newtype markers carry the package-root host selections threaded through their
saturated carrier; host-type markers need no such package-root arguments.
`ApplyN` privately boxes the already-erased body value. Its raw constructor and
projection stay package-private. The package handle exposes declaration-keyed
`KioNewtypeApplication_<declaration>_lift` / `_project` and
`KioHostTypeApplication_<declaration>_lift` / `_project` methods relating a
saturated carrier to its constructor application; those methods alone invoke
the declaration's ordinary newtype or host-binding adapters. No public
application type exposes `Object`, and the private body is neither walked nor
re-keyed.

A polymorphic newtype payload uses the same erased value representation but
retains one hidden callable stage per binder. A work-free stage returns the
next callable; a stage with computation performs it first. A direct statically
known call may compact adjacent leading type applications, but the stored
first-class value may not. Body construction and projection add no HKT-specific
conversion and remain identities around the complete callable; a public
wrapper performs the ordinary value-only function adaptation. None of these
internal stages adds a value parameter to the public Java facade
([`README.md` § Polymorphic newtype payloads](README.md#polymorphic-newtype-payloads)).

## Item naming

At the package surface, a root module and ordinary function/member leaves use
[shared word casing](README.md#source-derived-public-names) (`do_work` → `doWork`,
`Native_token` → `NativeToken`). A Java keyword or otherwise restricted
identifier uses `KioItem_<exact-component>`. Exact components apply word casing
before escaping remaining `_` as `_u`; exact paths also escape `/` as `_s`.
Nested modules always use `KioModule_<exact-component>` and public newtype
handles always use `KioType_<exact-component>`. Host-interface methods use
the readable/exact mangling of [§ Host record contract](#host-record-contract).
Structural shell and nominal carrier names follow [§ FFI surface](#ffi-surface).
Source-readable carrier, host-binding, callable-site, and `forall` owner
components use the same word casing and exact escaping. This changes their
host spelling, not the raw declaration identity or opaque structural shell
identity used to distinguish types.
An unsaturated nominal constructor uses
`KioNewtypeMk_<exact-module>__<exact-name>` or
`KioHostTypeMk_<exact-module>__<exact-name>` according to its declaration
category.

A first-class `forall` interface uses
`KioForall_<exact-owner>_Binders_K<n>_B<i>_A<a>...`: `<exact-owner>` is the
callable site or transparent-newtype declaration identity, `K<n>` frames the
consecutive leading binder count, and each `B<i>_A<a>` is that plan's stable
scoped binder identity plus kind arity. The component is therefore independent
of arena-use numbering and of other declarations. Through 64 characters this
reversible spelling is used directly; a longer identity uses
`KioForall_H<hex>` with the same complete-identity FNV-1a fallback and
collision check as structural shells. The long-name hash uses the raw
source-component identity frame, not its word-cased readable spelling.

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

pub fn main() -> . { print("hello from kio\n") }
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

# TypeScript backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `typed-dynamic` (see [`README.md` § Language families](README.md#language-families)).

> **Known host-source compatibility caveat.** When removed host signatures
> depend on incompatible epochs of one exact nominal declaration, the `.d.ts`
> omits those signatures and their conflicting type closure. Existing host
> source must delete or rewrite the affected optional properties and any
> import or annotation that names the superseded generated type. See
> [§ Incompatible retained declaration epochs are not source-stable on TypeScript](#incompatible-retained-declaration-epochs-are-not-source-stable-on-typescript).

The `"ts"` build target ([`specs/package.md` § Build target files](../package.md#build-target-files))
is the **typed-skin companion of the JS backend**. It emits the JavaScript
backend's package module *byte-identical* plus a generated TypeScript
declaration file (`.d.ts`) that types the FFI boundary, so a TypeScript host
calls into the package with full type-checking while the runtime artifact is
the same one a JS host runs. This page describes the surface a TypeScript host
calls against; everything about the runtime `.js` is governed by
[`js.md`](js.md), which this page does not restate.

## Language version

The `.d.ts` targets **TypeScript 5.0+** and is written to type-check cleanly
under `--strict` (`--noImplicitAny`, `--strictNullChecks`, …). The runtime
`.js` is the JS backend's, so its floor is the JS backend's **ECMAScript 2020**
([`js.md` § Language version](js.md#language-version)): a TypeScript host runs
the emitted `.js` in any ES2020 runtime (Node 14+, Deno, Bun, evergreen
browsers) with no build step — the `.d.ts` is purely additive, the standard
way a typed npm package ships type annotations for plain JavaScript.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target ts {
    out "out/ts/";
  }
}
```

`kio build ts` writes under the `out` directory:

- `<ns>.js` — the package's invocable surface as an ES module. **Byte-identical
  to the JS backend's `<ns>.js`** (same emitter, same bytes, same resolved
  namespace): the runtime body is shared with `target=js` and is drift-proof.
  Its contract is [`js.md`](js.md).
- `<ns>.d.ts` — the natural-exact FFI skin: TypeScript declarations for the
  `<ns>.js` surface. It exports `<Handle>HostTypes`, `<Handle>Host<B>`,
  `<Handle><B>`, `<Handle>Types<B>`, the structural `<Handle>TypeLambda` /
  `<Handle>Apply` / `<Handle>Bind` protocol, and the `create<Handle>` factory.
  This is the only artifact the `ts` backend adds over `js`.

`<ns>` is the artifact's **namespace** — its module stem, shared by the `.js`
and `.d.ts`: the kio package name by default, overridden by the `namespace`
build-block key ([`specs/package.md` § Per-target keys](../package.md#per-target-keys)),
an explicit value matching `[A-Za-z_][A-Za-z0-9_]*`. The branded names
(`<Handle>` = the namespace's public brand, `<Handle>HostTypes`, `<Handle>Host`,
`<Handle>Types`, `create<Handle>`, and the type-lambda helpers)
derive from it exactly as on the JS backend
([`js.md` § Output layout](js.md#output-layout)), so two packages' skins never
collide in one host program. There is no runtime-support file: the
`typed-dynamic` skin adds declarations only
([`README.md` § Runtime-support library](README.md#runtime-support-library)).

## Loading the package

A TypeScript host brings the package online exactly as a JS host does
([`js.md` § Loading the package](js.md#loading-the-package)) — the steps are
unchanged because the runtime is the same `.js`; the `.d.ts` only adds types.

### 1. Import the factory

```ts
import {
  create<Handle>,
  type <Handle>,
  type <Handle>Host,
  type <Handle>HostTypes,
  type <Handle>TypeLambda,
} from './out/ts/<ns>.js';
```

TypeScript resolves the sibling `<ns>.d.ts` for the imported `<ns>.js`
automatically. The single runtime export is `create<Handle>`; every other name
above is type-only.

### 2. Build the host record

The host argument is typed by `<Handle>Host<B>`: one member per
`host fn` the bridged modules declare, **nested by module** the same way the JS
host record is ([`js.md` § Build the host record](js.md#2-build-the-host-record)).
A path without source `_` replaces `/` with `_`, keeping lowercase components;
a path containing `_` uses `KioModule_<exact-path>`, with word casing before
escaping remaining `_` as `_u` and `/` as `_s`. Callable leaves use word
casing (`do_work` → `doWork`).

Roleless host types are selected separately by an interface `B` extending
`<Handle>HostTypes`. Its outer keys apply word casing to each module component
while retaining `/`, rather than using runtime ABI keys: `app` is a bare
property and `app/data_store` becomes the quoted key `"app/dataStore"`.
Its inner keys are word-cased type leaves (`Native_token` → `NativeToken`). A nullary
entry selects the native type directly; a positive-arity entry selects a
structural `<Handle>TypeLambda` witness (see § Higher-kinded types). Role types
are fixed TypeScript primitives and never appear as selectable entries.

```ts
interface ArrayK extends <Handle>TypeLambda<readonly [unknown]> {
  readonly type: readonly this["arguments"][0][];
}

interface AppTypes extends <Handle>HostTypes {
  readonly app: {
    readonly Token: NativeToken;
    readonly Array: ArrayK;
  };
}

const host: <Handle>Host<AppTypes> = {
  app: {
    print: (s) => { process.stdout.write(s); return null; },
  },
};
```

TypeScript checks both the binding map and the supplied record. Independently
generated packages use the same structural witness properties, so a host
witness or partial constructor can serve more than one artifact.

### 3. Instantiate and invoke

```ts
const pkg = create<Handle>(host); // infers AppTypes from the annotated host
pkg.app.KioModule_main.main();
```

`create<Handle>(host)` returns `<Handle<AppTypes>>`; the runtime behavior is
the JS backend's ([`js.md` § Instantiate and invoke](js.md#3-instantiate-and-invoke)).

## Package API

The value `create<Handle>` returns is typed by **`<Handle<B>>`**: the
bridged modules' `pub` items under the same role-framed namespace as the JS
package surface: the root module uses word casing, nested modules use
`KioModule_<exact-component>`, and public newtype handles use
`KioType_<exact-component>`
([`js.md` § Package API](js.md#package-api)). The `.d.ts` types this surface;
it does not change which items are present.

- One function-valued member per `pub fn`, typed from its full Kio scheme (see
  § FFI surface). Type arguments remain erased at runtime but surface as
  TypeScript generics: `pub fn id[T](x: T) -> T` presents as
  `id: <T>(p0: T) => T`. The direct facade combines separate declaration-head
  value groups into one call while retaining the declaration's source-value
  parameter partition. Each runtime-bearing source parameter remains one
  TypeScript parameter; a product-typed parameter remains one object parameter
  with its right-spine keys, while a canonical Unit-only domain is nullary.
  Callbacks, returns, and nested `forall`s remain first-class values and retain
  their own callable and generic layers.
- One namespace per `pub newtype`, selected as
  `KioType_<exact-component>` under its module namespace, whose typed
  members are the newtype's pub constructor and projector
  ([`js.md` § Public newtype namespaces](js.md#public-newtype-namespaces)).
  Each member is typed from its exact synthesized scheme. With both members
  public, a nonexistential newtype whose boundary recursion is not an uncut
  payload-visible cycle maps the payload to `{ <TypeName>: … }`; its projector
  maps that same structural type back to the payload. Existential projectors
  retain their exact generic CPS continuation. A newtype with a hidden member,
  an existential, or an uncut payload-visible recursive cycle instead uses the
  declaration-specific nominal handle from § FFI surface; the namespace
  exposes only the public member. A public newtype with no public members has
  an empty namespace type.

The `<Handle<B>>` shape is additive in the same sense as the JS surface — a
later kio version may add members; existing host call sites keep type-checking.

## Host record contract

`<Handle>Host<B>` mirrors the bridged modules' `host fn` surface, one nested
object per module that declares host fns and one readonly function-valued
member per `host fn`, using word-cased callable leaves
([`README.md` § 8 Host-trait descriptor](README.md#8-host-trait-descriptor)).
Nothing else is required — there are no implicit per-role operations, no
allocator hook, no ambient runtime; a package asks for a capability only by
declaring a `host fn`, and the host supplies exactly that.

For each `host fn <name>(p0: <args...>) -> <ret>;` declared in module `<m>`
(whose runtime namespace key is `<NS>` — the module key described above):

- `host.<NS>.<name>` is a property typed `<binders>(p0: <T0>, …) => <Ret>`,
  with each parameter and the return at its exact boundary type (see § FFI
  surface). Making it a function-valued property rather than an interface
  method preserves strict parameter variance under `--strictFunctionTypes`.
  Rank-N callbacks and returns remain generic function types.
- The host supplies the callable synchronously; TypeScript checks it against
  the declared signature.

A `host type` declaration carries **no runtime member** on `<Handle>Host<B>` —
host types are erased at the JS boundary and impose no host-record entry
([`js.md` § Non-atomic host types](js.md#non-atomic-host-types)). A `role(...)`
annotation on a host type only governs the TypeScript type of values of that
type at the boundary (see § FFI surface > Atomic types); it installs no
callable and creates no selectable type entry. Every roleless declaration has
one exact entry in `<Handle>HostTypes`, and every signature indexes the selected
`B` at that declaration identity.

When host fns are present, the valid factory branch is
`create<Handle>(host: <Handle>Host<B>)`; an annotated host lets TypeScript infer
`B`. When no host fn exists, the valid branch has no argument. If live roleless
slots exist in that hostless package, the host selects them explicitly with
`create<Handle<AppTypes>>()`. A bare call is accepted only when there are no
live selectable slots. Every factory, Host, Handle, and public type-catalog
path rejects an inherited base slot, a partial selection, `any`, `unknown`,
`never`, a wrong constructor arity, or a constructor with no concrete result.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) and
[`../versioning.md` § Deprecated host items](../versioning.md#deprecated-host-items)
for the TypeScript backend.

The TypeScript backend inherits the JS backend's runtime **removal tolerance**
([`js.md` § Deprecated host items](js.md#deprecated-host-items)): the `.js`
factory validates and dispatches only the host fns the *current* package
declares. The `.d.ts` additionally preserves source typing for a representable
removed host fn. Its exact frozen signature remains on `<Handle>Host<B>` as an
optional readonly function property preceded by TypeScript's recognized
`/** @deprecated ... */` JSDoc tag. An unchanged object literal therefore
keeps contextual parameter and result typing under `--strict`, while a new
host omits the property.

If a module path contains only retained methods, that outer Host property is
also optional and deprecated. A path that still contains a live method remains
required and nondeprecated, with only its retained leaves optional. Exact
callable identity uses the live-dominant provenance join: when current source
and sealed history both reach the same module/name, the one property is live,
required, and nondeprecated.

A removed host type disappears from current binding validation. When a
representable frozen signature reaches it, `<Handle>HostTypes` retains its
module path and declaration leaf as optional deprecated properties. A current binding map need
not select either one; an older map that still supplies them continues to
specialize the retained method's exact signature. Every other declaration
emitted solely for that signature — nominal carrier, type-lambda witness,
alias, or catalogue entry — carries recognized `@deprecated` JSDoc as well.
If a declaration is also reachable from the live facade, live provenance wins:
it stays required where applicable and is not deprecated merely because
history reaches it too.

### Incompatible retained declaration epochs are not source-stable on TypeScript

One TypeScript declaration path such as `Types<B>["api"]["Epoch"]` cannot
denote two incompatible exact payload declarations at once. If two retained
signatures require different frozen epochs of that identity, or a retained
epoch conflicts with the live declaration, the shared rule in
[`README.md` § Incompatible retained declaration epochs](README.md#incompatible-retained-declaration-epochs)
omits every affected retained root and its conflicting type closure. A union,
intersection, or widened carrier would change at least one exact signature.

A host written against an affected old artifact must delete or rewrite the
omitted optional function properties and replace any import or annotation that
names the superseded generated type. The emitted JavaScript was current-only
already, so this source edit adds no runtime obligation or package path.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs); its
comment cites the shared rule's explicit backend fan-out to this subsection.

For compatible retained epochs, the root `B` parameter remains on Host,
Handle, Types, namespaces, and the factory even after the last live slot is
removed, so old explicit maps remain source-compatible. Retained declarations
are type-only: they add no factory argument, binding validation, JavaScript
presence check, dispatch entry, package member, or body adapter. The emitted `.js` remains byte-identical to
the JavaScript target. The contravariant asymmetry the cross-backend idiom
notes still holds: adding a `host fn` adds a required `<Handle>Host` member
(and the runtime presence check), so it is a breaking env change.

Under [`README.md` § Facade topology and execution
provenance](README.md#facade-topology-and-execution-provenance), the `.d.ts`
may describe a retained callable only through that optional deprecated
property. The shared JS runtime still dispatches only to current host
functions. Calling a retained property directly is ordinary host code calling
the host's own extra function; no package execution path can reach it.

## FFI surface

How values cross the boundary, typed. The `.d.ts` types the **JS shape** every
value crosses in ([`js.md` § FFI surface](js.md#ffi-surface)) — the runtime is
the JS backend's, so the types describe exactly the JS plain-object / primitive
shapes the `.js` produces. Atomic role-typed values are TypeScript primitives;
structural values and payload-visible, nonexistential newtypes whose boundary
recursion has a finite structural spelling are TypeScript object / union types
retaining their generic relations. Member-hidden and existential newtypes, plus
payload-visible recursive cycles not cut by an atomic carrier, use private
nominal declarations matching the JS runtime carriers. A small set of
carve-outs (unit, function values, bottom) are TypeScript-native.

### Atomic types (governed by `role`)

A `role(...)` annotation on a `host type` makes that type a receiver for
literal tokens whose shape its role admits
([`specs/language.md` § Literals](../language.md#literals)). At the boundary the
TypeScript backend binds each role to the TypeScript type of the JS value shape
in [`js.md` § Atomic types](js.md#atomic-types-governed-by-role):

| Role                       | TypeScript type |
| -------------------------- | --------------- |
| `role(str)`                | `string`        |
| `role(bool)`               | `boolean`       |
| `role(i8)` … `role(i32)`   | `number`        |
| `role(u8)` … `role(u32)`   | `number`        |
| `role(i64)`, `role(i128)`  | `bigint`        |
| `role(u64)`, `role(u128)`  | `bigint`        |
| `role(f32)`, `role(f64)`   | `number`        |

`number` and `bigint` are distinct in TypeScript and in JS arithmetic (`1 + 1n`
throws at runtime), so the wide-integer roles type as `bigint` exactly because
the JS `.js` carries them as `BigInt` ([`js.md` § Atomic types](js.md#atomic-types-governed-by-role)).
A host passing a value outside a role's range gets the JS backend's
implementation-defined behavior; the type is a contract on shape, not on range.

### Non-atomic host types

A `host type Foo;` without a `role(...)` annotation is the direct slot
`B["<module>"]["Foo"]`. A declaration `host type Array[T];` is a positive-arity
slot constrained to `<Handle>TypeLambda<readonly [unknown]>`; `Array(A)` renders
as `<Handle>Apply<B["<module>"]["Array"], readonly [A]>`. Higher arities use
longer tuples, and a non-saturating prefix uses `<Handle>Bind`. These are
compile-time relations only: the JavaScript still carries the host's values by
reference with no type object or wrapper
([`js.md` § Non-atomic host types](js.md#non-atomic-host-types)). Two distinct
Kio declarations may deliberately select the same native type or witness; the
binding relation is declaration-keyed, not injective.

### Structural and nominal types

Values of structural type (built from `&`, `|`) and newtypes whose constructor
and projector are both public cross as the JS plain-object shapes
([`js.md` § Structural and nominal types](js.md#structural-and-nominal-types)),
typed in the `.d.ts` as **anonymous TypeScript object / union types** keyed by
the right-spine slot. The shape is determined by the source-level type, walked
over the right spine of `&` and `|`
([`README.md` § The right-spine walk](README.md#the-right-spine-walk)):

- `()` (unit) — `null`.
- `!` (bottom) — `never` (no inhabitants; never crosses).
- A `role(...)` type — its TypeScript primitive per the table above.
- An ordinary type variable (`[A]`-quantified) — the corresponding TypeScript
  generic `A`. A higher-kinded binder is a constrained structural witness;
  its applications use `Apply` / `Bind`.
- A function type — a TypeScript function type (see § Carve-outs).
- A `newtype` with payload `X` and both members public —
  `{ [k]: shape(X) }` where `[k]` is the newtype's FFI key (its type name)
  per the [3-step fallback](README.md#3-step-key-fallback). `labels` sugar
  and an equivalent explicit `newtype` use this same shape. An uncut
  payload-visible recursive cycle is the exception (**Recursive newtypes**
  below).
- A public `newtype` with fewer than two public members, an existential, or a
  payload-visible recursive cycle not cut by an atomic carrier — a generated
  nominal class type with a declaration-private brand and no public instance
  members.
  The declaration is not exported as a host-namable type; it appears in public
  signatures so TypeScript can infer and shuttle package-produced values, but
  a host cannot construct a structurally matching value or inspect a payload.
  Distinct Kio declarations have distinct TypeScript types even when their
  leaf names and payload shapes coincide. This types the JS backend's exact,
  declaration-specific hidden handle.
- A product type `(A & B)` — `{ <k0>: shape(A); <k1>: shape(B) }`, every
  right-spine slot a property.
- A sum type `(A | B)` — a **discriminated union** `{ <k0>: shape(A) } | { <k1>:
  shape(B) }`, exactly one property set, identifying the inhabited slot.

**Recursive payload-visible newtypes.** Recursion is classified at the JS
boundary. A payload-visible cycle that reaches itself directly (`List : . |
(I32 & List)`) or only through other payload-visible newtypes has no finite
structural value walk, so it crosses as the package's opaque internal carrier
([`js.md` § Structural and nominal types](js.md#structural-and-nominal-types)).
The `.d.ts` gives that carrier a declaration-private nominal type parameterized
invariantly by `B` and every universal parameter. The host can construct and
project it through the public members and round-trip it precisely, but cannot
inspect or forge the carrier. Existential carriers follow the same nominal
rule while omitting the hidden existential from their nominal parameters; the
projector reveals it only to its generic CPS continuation.

An already atomic boundary carrier, such as a public opaque or one-member
newtype, cuts that recursion walk. Therefore an enclosing payload-visible
wrapper such as `Root : Hidden(Root)` remains the structural
`{ Root: Hidden<Root> }` JS shape rather than becoming nominal. The `.d.ts`
uses a private structural interface only as a finite name for the recursive
reference; the interface has no brand and does not remove the host's ability
to construct the public object shape.

Per-slot key naming follows the [3-step fallback in
`README.md`](README.md#3-step-key-fallback): unqualified newtype name →
canonical `<modulepath>.<TypeName>` → positional `_<n>`. TypeScript-specific
instantiation: steps 1 and 3 are legal property identifiers; the step-2
qualified spelling carries a `.`, which is a legal property key only as a quoted
string literal, so the `.d.ts` renders it `"<modulepath>.<TypeName>"` (a quoted
key), matching the JS object key the host already uses with bracket access.
Both named steps word-case the type leaf and each module component while
retaining route `/` and `.` (`data_api/model.Native_token` →
`"dataApi/model.NativeToken"`). Positional keys remain `_<n>`.

Worked examples (assume `labels { t1 : I32, t2 : Str };` declares generated
types `T1`, `T2`; `I32`, `Str` are role-typed host types):

| Kio type            | TypeScript type                         |
| ------------------- | --------------------------------------- |
| `T1`                | `{ T1: number }`                        |
| `T1 & T2`           | `{ T1: number; T2: string }`            |
| `(I32 & Str)`       | `{ _0: number; _1: string }`            |
| `(T1 \| T2)`        | `{ T1: number } \| { T2: string }`      |
| `(I32 \| Str)`      | `{ _0: number } \| { _1: string }`      |

A host narrows a sum by checking which property is present, exactly as on JS
(`"T1" in v ? … : …`), and the discriminated union lets TypeScript narrow the
type in each branch. The type describes the public JS shape, not the package's
internal rep — hosts must not introspect an internal shape across kio versions
([`js.md` § Conversion semantics](js.md#conversion-semantics)).

### Carve-outs

Three kinds of value are TypeScript-native, mirroring the JS carve-outs
([`js.md` § Carve-outs](js.md#carve-outs)):

- **Unit `()`** is `null`.
- **Function values** are TypeScript function types `(p0: <T0>, …) => <R>`,
  called synchronously with the callable's canonical source-parameter
  partition. A runtime-bearing parameter whose type is a product is therefore
  one structural object, while a canonical Unit-only domain is nullary. Each
  parameter and the return follows whichever rule applies to its type.
  (Parameters are named `p0`, `p1`, … in the `.d.ts` because TypeScript
  requires names in a function-type position.)
- **Bottom `!`** is `never`. A `fn` declared `-> !` is contractually obliged not
  to return normally; a `!`-typed return from package code is unreachable in
  well-typed programs.

These carve-outs are stable across implementations.

## Higher-kinded types

TypeScript is in the `typed-dynamic` family, whose HKT strategy is the
**natural-exact skin**
([`README.md` § Higher-kinded types](README.md#higher-kinded-types)). The
runtime remains the JS backend's byte-identical, type-erased `.js`: an
application `F(A)` has no extra runtime representation, lift / project remain
identity, and no type witness is passed at runtime
([`js.md` § Higher-kinded types](js.md#higher-kinded-types)). The `.d.ts`
preserves the constructor relation structurally:

```ts
export interface <Handle>TypeLambda<Args extends readonly unknown[]> {
  readonly arguments: Args;
  readonly __kio_type_lambda_variance?: (args: Args) => Args;
}
```

The base protocol deliberately has no `type` member. A native constructor
witness extends it at its exact arity and supplies a result through
`this["arguments"]`:

```ts
type NativeArray<A> = readonly A[];

interface ArrayK extends <Handle>TypeLambda<readonly [unknown]> {
  readonly type: NativeArray<this["arguments"][0]>;
}
```

Naming a positive-arity host type or higher-kinded binder by itself renders
its witness. Applying all arguments renders
`<Handle>Apply<F, readonly [A, ...]>`; supplying a nonempty,
non-saturating prefix renders `<Handle>Bind<F, readonly [A, ...]>`. `Bind`
records the root and accumulated prefix structurally, so nested partials
compose across independently generated packages. Tuple recursion computes
arity and remaining arguments without a fixed maximum, and each argument's
constraint retains the Kio kind's complete arrow-domain shape.

`Apply` is fail-closed. It returns a branded, invariant, uninhabited diagnostic
type for a bare base protocol, `any`, `unknown`, `never`, a non-witness, a
non-tuple or wrong-arity argument list, or a witness whose result is missing,
`any`, `unknown`, or `never`. It never turns any of those failures into
`unknown` or `never`. This includes public Kio signatures abstract over
`*F`: selecting the bare generated base protocol for `F` is diagnosed even
though factory binding validation does not govern a caller-chosen `F`.

Witnesses and partials use the same public structural property names in every
artifact. A host may therefore author a witness independently, reuse it across
packages, and deliberately select the same witness for distinct Kio
declarations. The generated `<Handle>Types<B>` catalogue makes reachable role
types, selected host types, and public Kio newtype constructors available by
their readable declaration paths for generic host-side composition.

**Polymorphic newtype payloads.** The dictionary pattern
(`pub newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B)`) needs no special
HKT representation in the `.js`: bound types erase at runtime, but the stored
callable retains the application stages described in
[`README.md` § Polymorphic newtype payloads](README.md#polymorphic-newtype-payloads).
Every binder of this first-class value remains one hidden callable stage; a
work-free stage returns the next callable, while a stage with computation
performs it first. A direct known call may separately compact adjacent leading
type applications. With both members public, the constructor and projector use
the ordinary structural newtype boundary. Inside the JavaScript body those
operations are identities around the complete staged callable; the public
wrapper performs the ordinary value-only function adaptation. At the `.d.ts`,
that case surfaces as `{ <ffi_key>: <payload type> }` (§ FFI surface), with
every ordinary and higher-kinded `forall` retained as a TypeScript generic
function layer and every application rendered through `Apply` / `Bind`.
Hiding either member instead selects the exact invariant nominal carrier; the
hidden payload remains the same stored closure.

## Item naming

Items appear on the `<Handle>Host<B>` type (`host.<NS>.<name>`) and on the
`<Handle<B>>` type under their declaring module's role-framed namespace and
their word-cased Kio leaf — the same naming as the JS backend
([`js.md` § Item naming](js.md#item-naming)). The `<NS>` key is the module path
formed with `/` replaced by `_` when the path contains no source
underscore, and `KioModule_<exact-path>` otherwise. The package surface keeps
the word-cased root module, uses `KioModule_<exact-component>` for nested
modules, and `KioType_<exact-component>` for public newtype handles.

TypeScript uses [shared word casing](README.md#source-derived-public-names)
(`do_work` → `doWork`, `Native_token` → `NativeToken`).
Exact components escape remaining `_` as `_u` after that conversion.
Host-type binding maps and the type catalog retain `/` between word-cased
module components and use word-cased type leaves; the nominal hexadecimal
identity remains based on the raw declaration identity.

TypeScript, like JS, permits reserved words at
property / member positions (`pkg.return`, `pkg.class`, `pkg.delete` are legal),
and these names appear only at property-access positions. A leaf name that is
not a legal bare TypeScript identifier (e.g. the step-2 qualified `.`-bearing
FFI key) is rendered as a quoted property key; quoting adds no further
name transformation.

## Worked example

Given a package `greeter` with:

```kio
// greeter.pkg.kio
package greeter;

build {
  cache "out/.kio-cache/";

  target ts {
    out "out/ts/";
  }
}

bridge {
  greeter;
  greeter/**;
}
```

```kio
// greeter.kio
module greeter;

host type String role(str);
host fn print(s: String) -> .;
```

```kio
// greeter/main.kio
module greeter/main;

import greeter(String, print);

pub fn main() -> . { print("hello from kio\n"(String)) }
```

`kio build ts` writes `out/ts/greeter.js` (byte-identical to `target=js`) and
`out/ts/greeter.d.ts`. The `.d.ts` declares (shape illustrative):

```ts
export interface GreeterHostTypes {}

export type GreeterHost<
  B extends GreeterHostTypes = GreeterHostTypes,
> = {
  readonly greeter: {
    readonly print: (p0: string) => null;
  };
};

export type Greeter<
  B extends GreeterHostTypes = GreeterHostTypes,
> = {
  greeter: {
    KioModule_main: {
      main: () => null;
    };
  };
};

export function createGreeter<B extends GreeterHostTypes = GreeterHostTypes>(
  host: GreeterHost<B>,
): Greeter<B>;
```

A TypeScript host invokes the package as:

```ts
import { createGreeter, type GreeterHost } from './out/ts/greeter.js';

const host: GreeterHost = {
  greeter: {
    print: (s) => { process.stdout.write(s); return null; },
  },
};

const pkg = createGreeter(host);
pkg.greeter.KioModule_main.main();
// → writes "hello from kio\n" to stdout
```

### Structural values in action

A package exposing a fn over structural products lets the host build and unpack
values directly as typed objects:

```kio
// pair_swap/main.kio
module pair_swap/main;

import __intrinsics__;
import pair_swap(I32, String);

pub fn pair_swap(p: (I32 & String)) -> (String & I32) {
  __pair__(String, I32, __snd__(I32, String, p), __fst__(I32, String, p))
}
```

```ts
import { createPairSwap } from './out/ts/pair_swap.js';
const pkg = createPairSwap();

const p: { _0: number; _1: string } = { _0: 42, _1: "hello" };
const q = pkg.pairSwap.KioModule_main.pairSwap(p); // typed { _0: string; _1: number }
console.log(q._0, q._1);                      // "hello" 42
```

The host constructs and inspects values via the same key shape on both sides,
type-checked; the runtime conversion is the JS backend's, transparent.

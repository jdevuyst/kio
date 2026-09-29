# JavaScript backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `dynamic` (see [`README.md` § Language families](README.md#language-families)).

The `"js"` build target ([`specs/package.md` § Build target files](../package.md#build-target-files))
emits JavaScript. This page describes the surface the host must call against.

## Language version

Emitted code targets **ECMAScript 2020**, packaged as an ES module. The two
gating features are **module syntax** (`export function …`) and `BigInt`
literal syntax (`42n`, emitted for any ≥64-bit integer literal in source).
All other constructs are ES2015 (arrow functions, `const` / `let`, template
literals).

In runtime terms that means Node 14+, Deno, Bun, and all evergreen browsers
from 2020 onward.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/";
  }
}
```

`kio build js` writes under the `out` directory:

- `<ns>.js` — the package's invocable surface as an ES module.
  Always exactly one. No internal sibling files; every Kio module gets
  inlined into the entry's `create<Handle>` factory body.

`<ns>` is the artifact's **namespace** — its module stem: the kio
package name by default, overridden by the `namespace` build-block key
([`specs/package.md` § Per-target keys](../package.md#per-target-keys)).
An explicit value must match `[A-Za-z_][A-Za-z0-9_]*`; anything else is
a build error. A package name is always a legal stem, so the default
never needs escaping.

The factory is `create<Handle>`, using the
[shared public brand](README.md#branded-naming) of the effective namespace,
not the source package name after an override. Namespace `my_pkg` retains
artifact `my_pkg.js` and exports `createMyPkg`; `_my_pkg` exports
`createKioPkg__uMyPkg`; explicit `MyPkg` exports `createKioNs_4d79506b67`.
Because the one
exported name carries the brand, two kio packages with distinct
namespaces load into one host program without collision (JS is dynamic —
the package handle and host record are plain values, so only the factory
name is a top-level symbol).

## Loading the package

A host that wants to call into the package performs three steps in order.

### 1. Import the factory

```js
import { create<Handle> } from './out/js/<ns>.js';
```

The single named export is the branded `create<Handle>` factory
(§ Output layout) — synchronous and side-effect-free. Importing the
module produces no observable effect on the host — no globals are
written, no I/O runs. The package only comes online when the factory is
invoked.

### 2. Build the host record

The host argument is a plain JS object that supplies one callable per
`host fn` the bridged modules declare. The record is **namespaced by
module**. A path with no source `_` keeps the readable key obtained by
replacing `/` with `_`, retaining lowercase source components. A path
containing `_` instead uses `KioModule_<exact-path>`: apply word casing
componentwise, then encode remaining `_` as `_u` and `/` as `_s`.
Thus `foo/bar` uses `foo_bar`, while `foo_bar` uses `KioModule_fooBar`;
the two declarations can coexist. Callable leaves use the same word casing:
`do_work` becomes `doWork`. A `host fn print` declared in module `app`
is supplied as `host.app.print`. `host type`
declarations are erased at runtime and require nothing of the host.

```js
const host = {
  app: {
    // one property per `host fn` declared in module `app`
    print: (s) => { process.stdout.write(s); return null; },
  },
  // … one nested object per module that declares host fns
};
```

### 3. Instantiate and invoke

```js
const pkg = create<Handle>(host);
pkg.app.KioModule_main.main();
```

`create<Handle>(host)` validates the supplied record, copies the items it
cares about into a frozen internal record, evaluates the package's modules
in dependency order, and returns the package's exposed surface as a plain
record. The first module segment uses word casing. Every nested
module segment uses `KioModule_<exact-component>`, and every public newtype
handle uses `KioType_<exact-component>`; word casing precedes escaping
remaining `_` as `_u`. An export `main`
from module `app/main` is therefore reached as
`pkg.app.KioModule_main.main`.

```js
// app/main.kio declares `pub fn main() -> .;`, module app/main is bridged
pkg.app.KioModule_main.main();
```

Exporting a public type exposes that type's pub members atomically
(see [`specs/package.md`](../package.md#package-file));
members are reachable under the type's module namespace through its
`KioType_<exact-component>` selector.

## Host record contract

The host record's shape mirrors the bridged modules' `host fn` surface: one
nested object per module that declares host fns, and one property on that
object per `host fn`. Nothing else is required — there are no implicit
per-role operations (no built-in `add_i32`, no `int_to_string`, no `panic`),
no allocator hook, no ambient runtime. If a package wants something from its
host, a module must declare it with a `host fn`; the host then supplies
exactly that.

For each `host fn <name>(p0: <args...>) -> <ret>;` declared in module `<m>`
(whose namespace key is `<NS>` — the readable or exact module key described
above), let `<name>` below denote its word-cased host leaf:

- `host.<NS>.<name>` is a JS function called synchronously with positional arguments.
- Argument and return types follow the FFI value layout below.
- The host is responsible for honoring the item's declared semantics; the type system commits to type and arity, not to behavior.

`host type` declarations carry no runtime obligation — types are erased at
runtime. A `role(...)` annotation on a host type only governs how literal
tokens that resolve to that role are encoded at the FFI boundary (see
below); it does not require the host to install any functions and does
not create a canonical JS-side type object.

**Construction-time validation.** The `create<Handle>` factory checks that
every declared `host fn` is present on the supplied `host` argument under its
module sub-record and throws `Error("missing host item: <NS>.<name>")` otherwise.
Whether a leaf value is actually a function isn't checked —
the host's first call will surface a JS `TypeError` if the value is
wrong-shaped, which is a clean enough signal. Extra properties on `host`
are tolerated; the host may pass a superset for forward compatibility.

**Isolation.** The factory builds a fresh, frozen internal record by
walking the declared host fns once and reading each from the supplied
`host` under its module namespace. Caller-side mutations to `host` after
the factory returns don't leak into package code.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) for JS.

The JS backend is **removal-tolerant**, so it re-emits nothing for a removed host
item. Removing a `host fn` is a compatible env-side change at the language level
(see [`../versioning.md` § The compatibility
relation](../versioning.md#the-compatibility-relation)), and on JS it is also
source-stable with no codegen support: the factory only validates that the
host fns the *current* package declares are present
([§ Host record contract](#host-record-contract)), and extra properties on the
supplied `host` object are tolerated. So a host that still supplies a property
for a now-removed `host fn` keeps working unchanged — the package simply never
reads it. Removing a `host type` is likewise a no-op: host types are erased at
runtime and impose no host-record entry to begin with
([§ Non-atomic host types](#non-atomic-host-types)).

JS remains **addition**-breaking, exactly as the cross-backend idiom notes: a
new `host fn` adds a construction-time presence check, so a host that doesn't
supply it makes the factory throw `Error("missing host item: <NS>.<name>")`.
The contravariant asymmetry — removals tolerated, additions breaking — holds on
JS as on every backend; only the *removal*-side retention is backend-specific, and
the Rust backend's `#[deprecated]` re-emit
([`rust.md` § Deprecated host items](rust.md#deprecated-host-items)) has no JS
analogue because JS needs none.

## Boundary semantics

This backend inherits the cross-cutting properties enumerated in
[`README.md`](README.md) — synchronous calling, type erasure,
FFI additivity, package isolation, exception propagation,
well-foundedness inheritance, behavioral additivity. The notes
below cover only the JS-specific instantiations and details.

**Facade topology and execution provenance.** Under the shared
[`README.md` rule](README.md#facade-topology-and-execution-provenance), the JS
facade and host record use the current callable declarations only. A removed
host function contributes neither a facade property nor a package dispatch
entry; an extra property left on the host object remains host-owned data that
the package never reads. One package-complete preparation fixes every live
`host fn`, exported function, and public-newtype member together with its
source-parameter execution layout. JS keeps one public object argument for a
source parameter whose direct type is a product, then reconstructs that one
internal argument from the object's prepared right-spine keys; declaration
value groups are still flattened into one public call. A directly written
canonical Unit domain is nullary. When substitution changes an existing
non-Unit source slot to Unit, that slot instead remains one explicit `null`
argument. Nominals are
routed by their exact declaring module and type name from that same
preparation; adding an unrelated same-leaf declaration cannot change an
existing adapter.
`!` remains an impossible facade slot, so a product or sum containing `!`
still receives its ordinary structural JS conversion rather than becoming an
opaque compile-time value. These rules change only generated adapters: host
types and their type arguments remain dynamically represented, with no exact
host-binding syntax added to JavaScript.

**Synchronous calling.** A host that calls `pkg.x()` blocks until
`x` returns, and package code that calls `__host__.y()` blocks until
`y` returns. The package runs on whichever thread invoked it,
inheriting JS's single-threaded-per-realm model. Across re-entrant
calls the package retains no globally-mutable state — only the
frozen host record (built at factory-call time) and the
lexically-scoped module closures.

**Type erasure.** A `fn run[A](cfg: Config(A)) -> A`
lowers to the `run(cfg)` method under its role-framed module namespace; a
`host fn` declared with a rank-N
parameter type (`f: [T] T -> T`) receives only the value
argument; a Kio function value of polymorphic type, when passed
to a host callback, is a JS function the host calls with value
arguments only.

**Exception propagation.** A `host fn` that throws propagates its
exception unchanged: it unwinds through the Kio call stack and
emerges from the originating `pkg.x()` invocation. The package wraps
no host call in `try`/`catch`. Symmetrically, a `pkg.x()` call that
itself throws (most commonly the `__absurd__` runtime guard described
under § Carve-outs, or a `host fn` the package invoked) unwinds through
the host call stack the same way. Hosts that need to isolate package
failures wrap their `pkg.x()` calls in `try`/`catch` themselves.

**Value identity and lifetime.** FFI-shape values produced by package
calls (the JS objects whose keys carry the right-spine slots — see §
FFI surface > Structural and nominal types) have no specified
reference identity: two calls that happen to produce
structurally-equal values may return distinct JS objects. Hosts may compare them
with deep-equal helpers (`lodash.isEqual` / JSON-shape comparison)
since the JS shape is a public contract; identity equality (`===`,
`Object.is`) is **not** preserved across calls. The package retains no
implicit references to ordinary structural values the host passes into
package calls — once the call returns, the package holds nothing live
on the host's behalf. There are two exceptions: the frozen host record
captured at construction, and the private payload associated with a
live nominal handle. Nominal-handle storage is weakly keyed, so the
storage does not keep the handle alive and its payload becomes
collectible with the handle.

## FFI surface

How values cross the JS boundary, in three categories. Atomic
role-typed values flow as JS-native primitives. Structural values and
newtypes whose constructor and projector are both public flow as
**plain JS objects** keyed by the right-spine slot; a public newtype that
hides either member flows as a declaration-specific nominal handle (see
§ Structural and nominal types below). A small set of carve-outs
(unit, function values, bottom) flow as JS-native by design.

### Atomic types (governed by `role`)

A `role(...)` annotation on a `host type` makes that type a receiver for
source-level literal tokens whose shape its role admits (see
[`specs/language.md` § Literals](../language.md#literals)). At the JS
boundary, the JS backend binds each role to this value shape:

| Role                       | JS shape                                        |
| -------------------------- | ----------------------------------------------- |
| `role(str)`                | JS `string`                                     |
| `role(bool)`               | JS `boolean` (`true` / `false`)                 |
| `role(i8)` … `role(i32)`   | JS `Number` (integer, in the role's range)      |
| `role(u8)` … `role(u32)`   | JS `Number` (non-negative integer)              |
| `role(i64)`, `role(i128)`  | JS `BigInt` (signed, two's-complement at width) |
| `role(u64)`, `role(u128)`  | JS `BigInt` (non-negative)                      |
| `role(f32)`, `role(f64)`   | JS `Number`                                     |

`Number` and `BigInt` are not interchangeable in JS arithmetic — `1 + 1n`
throws `TypeError` — so a `host fn` that takes a `role(i64)` argument
must accept a `BigInt` literal (`42n`), not a plain `Number` (`42`).

**Out-of-range and non-integer inputs.** A host that hands a value
outside the role's range — e.g., `1.5` for a `role(i32)` slot, `2n ** 64n`
for `role(u64)`, or `NaN` for `role(f64)` — gets implementation-defined
package behavior. The Kio package performs no defensive coercion or
validation; ensuring a value belongs to its role is the host's
responsibility.

**String surface.** A `role(str)` JS string may contain any code point,
including lone surrogates and code points outside the BMP — the
`string` type imposes no restrictions and the package preserves
whatever it receives. Source-level string literals inside the package
are restricted to the escape grammar in [`specs/language.md`](../language.md#literals)
(BMP-only `\uXXXX`); values flowing in from host calls, projections,
or package returns are unrestricted.

### Non-atomic host types

A `host type Foo;` declaration without a `role(...)` annotation —
"non-atomic" because there is no primitive role to pass through to
— surfaces in JS as a **plain reference**. The host picks any JS
value of its choosing to represent `Foo`-typed values; the package
threads those references unchanged through any `Foo`-typed
argument, return, or compound-position slot.

JS's dynamic typing means the backend has nothing to bookkeep here.
A `host type Foo;` adds no entry to the host record (in contrast
to a `host fn`, which requires a named property on the host object):
the host's representation is whatever the host's `host fn`
implementations produce and accept. There is no constructor, no
shape declaration, no per-type slot to wire up.

Polymorphic non-atomic host types (`host type Array[T];`) flow
identically: the host's representation handles every instantiation,
and the type-parameter is erased to its host-chosen reference
identity. Contrast the Rust backend, where the polymorphic case
requires a Generic Associated Type on the `Host` trait — see
[`specs/backends/rust.md` § Non-atomic host types](rust.md#non-atomic-host-types).

The host's only obligation is **consistency across calls**. A
`Foo`-typed value the package returns to the host and later
receives back must be accepted as the same reference; identity
or equality bookkeeping happens on the host side, not inside the
package.

### Structural and nominal types

Values of structural type (built from `&`, `|`) cross the FFI as **plain JS
objects**. A `newtype` uses that same structural representation exactly when
both its constructor and projector are public. If either member is hidden, a
structural payload object would expose the missing operation implicitly: a
host could construct the nominal by passing the object in, or project it by
reading the object on the way out. Such a newtype therefore uses an opaque
nominal handle instead. This rule is derived only from the lowered newtype's
member visibility; `labels` sugar and an equivalent explicit `newtype` are not
distinguished.

The shape of a structural value is determined by its source-level
type, walked over the right spine of `&` and `|`:

- `()` (unit) — JS `null`.
- `!` (bottom) — has no inhabitants; never crosses the boundary.
- A `role(...)` type — its JS-native shape per the role table
  above.
- A function type — a JS function (see § Carve-outs).
- A type variable (`[A]`-quantified) — passes through whatever the
  host hands in; type erasure at the boundary makes the polymorphism
  invisible.
- A `newtype` with payload `X` and both members public — a JS object
  `{ [k]: shape(X) }` where `[k]` is the newtype's FFI key (its type name)
  per § FFI keys for newtype slots below. A newtype minted by
  `labels { f : X };` and an explicit
  `newtype F : X { pub constructor mk_f; pub projector f; };` therefore
  have the same shape. A recursive newtype is the one exception
  (**Recursive newtypes** below).
- A public `newtype` with fewer than two public members — a frozen,
  null-prototype nominal handle with no payload property. The package factory
  keeps the payload in private weak storage keyed by the exact declaring
  `(module, type name)` identity. A handle is accepted only by the package
  instance and declaration that minted it; a forged or wrong-declaration
  handle throws `TypeError`. The host may retain and shuttle a handle but
  cannot inspect its payload. This is an atomic leaf when nested in a product,
  sum, function, or another payload: enclosing conversion never descends into
  the hidden payload.
- A product type `(A & B)` — a JS object whose keys come from the
  right-spine walk: each spine slot contributes one key, populated
  by the slot's payload value.
- A sum type `(A | B)` — a JS object with **exactly one** key set,
  identified by the right-spine slot the value inhabits.

The right-spine walk algorithm is shared across backends — see
[`README.md` § The right-spine walk](README.md#the-right-spine-walk)
for the rule. JS-specific examples: `((A & B) & C)` flows as
`{_0: shape(A & B), _1: shape(C)}` (left-nested, two slots);
`(A & (B & C))` flows as `{_0: shape(A), _1: shape(B), _2: shape(C)}`
(right-nested, three slots, flattened).

**Recursive payload-visible newtypes.** A newtype whose constructor and
projector are both public and whose payload transitively reaches
itself — directly (`List : . | (I32 & List)`) or through a chain of
mutually recursive newtypes — has no finite structural shape. Such a
value crosses the boundary as the package's **type-erased internal
rep**, *not* a `{ [k]: shape(X) }` object: the host receives an opaque
carrier it round-trips through the package's own constructors /
projectors and must not introspect. A member-hidden newtype already uses its
nominal handle regardless of recursion. Only a non-recursive newtype with both
members public surfaces structurally. This is the JS analogue of the erased-carrier
rule `specs/backends/rust.md` states for a recursive newtype; both
follow from the value's Kio' shape alone.

#### FFI keys for newtype slots

Per-slot key naming follows the [3-step fallback in
`README.md`](README.md#3-step-key-fallback): unqualified newtype
name → canonical fully-qualified `<modulepath>.<TypeName>` →
positional `_<n>`.
JS-specific instantiation:

- Step 1 (unqualified) names word-case the source type component
  (`Native_token` → `NativeToken`). Step 3 keeps its positional `_<n>`.
  Both are legal JS
  identifier or property keys; rendered as dotted access (`v.F`,
  `v._0`).
- Step 2 (qualified) word-cases each source module/type component, retaining
  `/` and `.` as route delimiters. JS accepts the resulting string as an
  object key; hosts use bracket access (`v["dataApi/model.NativeToken"]`
  for `data_api/model.Native_token`).

Worked examples (assume `labels { t1 : I32, t2 : Str, t3 : I32 };`
declares generated types `T1`, `T2`, `T3`;
`I32`, `Str` are role-typed host types):

| Kio type                                   | FFI shape                          |
| ------------------------------------------ | ---------------------------------- |
| `T1`                                       | `{T1: <i32>}`                      |
| `T1 & T2`                                  | `{T1: <i32>, T2: <str>}`           |
| `(I32 & Str)` (no labels)                  | `{_0: <i32>, _1: <str>}`           |
| `(T1 & Str)`                               | `{T1: <i32>, _1: <str>}`           |
| `(T1 \| T2)`                               | `{T1: <i32>}` or `{T2: <str>}`     |
| `(I32 \| Str)`                             | `{_0: <i32>}` or `{_1: <str>}`     |
| `F(I32) & F(Str)` (parametric same label)  | `{F: <i32>, "m1.F": <str>}`        |

The host dispatches a sum value by checking which key is present:
`if ('T1' in v) … else if ('T2' in v) …`.

#### Conversion semantics

The package and the host agree on the JS-shape contract above.
Internally, the package may use any rep it likes (today's emitter
uses nested binary arrays); the per-signature wrapper at every package
fn boundary converts between internal-rep and JS-shape on the way
in and out. Hosts must not introspect a value of structural type
across kio versions assuming a particular *internal* shape — only the
JS-shape above is the public contract.

**Equality.** Hosts may compare two FFI-shape values using a
deep-equality helper (`lodash.isEqual`, JSON-shape comparison,
recursive-walk-by-key). The JS shape is a public contract, so
structural equality is well-defined. Identity equality (`===`) is
*not* preserved across calls: two calls returning structurally-equal
values may return distinct JS objects.

**Mutation and aliasing.** Conversion materializes fresh JS objects on
both sides. A host that mutates a passed object after the call does
not affect the package's internal state, and a JS object the package
returned is owned by the host. Atomic role-typed values are
immutable in JS so the question is moot for them; host-typed values
without a `role` annotation pass through by reference (the host's
concern, not Kio's).

### Carve-outs

Three kinds of value flow as JS-native — not as a structural-shape
JS object — because they have to:

- **Unit `()`** is JS `null`. Construct and match as JS `null` directly; there's no construction wrapper because there's nothing to construct.
- **Function values** (`(A & B & ...) -> R`) are JS functions, called positionally and synchronously. Arguments and return values follow whichever rule applies (atomic, structural, unit, function) to their respective types.
- **Bottom `!`** has no JS value. A `fn` declared `-> !` is contractually obliged not to return normally — typically by throwing, calling `process.exit`, or looping indefinitely. A normal return from a `-> !` host fn is undefined behavior at the boundary: Kio code may have eliminated the value via the inline `__absurd__` lowering (which throws a runtime guard) or omitted handling for it entirely. A `!`-typed *return from package code* is unreachable in well-typed programs.

These carve-outs are stable across implementations.

## Package API

The value returned by `create<Handle>(host)` exposes the bridged modules'
`pub` items under a role-framed form of their declaring module path. The root
module segment uses word casing; nested modules use
`KioModule_<exact-component>`; public newtype handles use
`KioType_<exact-component>`. Function leaves use word casing.
These disjoint selector classes let a function, nested module, and public
type share a source stem without one hiding another. A multi-value-group export's
record entry takes every group's parameters flat in one call — the wrapper
applies the internal curried layers group by group
([`README.md` § Function-type FFI canonicalization](README.md#function-type-ffi-canonicalization)). The shape is additive — future
kio versions may add new exports at the same level, and widening the bridge
globs sweeps in more modules — but hosts should consume only what their
package's bridged modules declare.

### Exports

One method (or property) per `pub fn`, plus one
`KioType_<exact-component>` handle for each `pub` type, all under the
declaring module's role-framed namespace. Function leaf names match the Kio
declaration after word casing (`do_work` → `doWork`):

```js
// module app/main declares `pub fn main() -> .;`, module app/main is bridged
pkg.app.KioModule_main.main();
```

Because exports keep their module namespace, two `pub fn main`s in different
bridged modules (`a/main` and `b/main`) are distinct entries
(`pkg.a.KioModule_main.main` and `pkg.b.KioModule_main.main`); there is no
leaf-name collision and no rename.

**Polymorphic exports.** Type arguments are erased at codegen, so a
polymorphic `pub fn` presents as a JS function taking only its value
parameters. Given a `pub fn id[T](x: T) -> T { id_impl(T, x) }` in module
`m`, a host calls

```js
pkg.m.id("hello");        // <t> is not passed; only the value argument
```

The same rule applies to rank-N value parameters and to function values
flowing in either direction (see § Boundary semantics — type erasure at
the boundary).

**Type exports.** A `pub` type `Bar` in a bridged module `m` whose target is a
`newtype` exports the newtype's pub members atomically: members are
reachable as `pkg.m.KioType_Bar.<member>`. A target that
is an alias has no runtime representation — type aliases erase entirely
— and surfaces nothing at `pkg.m.KioType_Foo`. To expose an alias-shaped surface
the package either declares a `newtype` and points the package code at that, or
exposes individual `fn`s that operate on the underlying
structural type.

### Public newtype namespaces

For each public newtype in a bridged module `m`, the package exposes a
namespace object `pkg.m.KioType_<exact-component>` whose properties are the
newtype's pub members (constructor and projector), under the declaring
module's namespace. Word casing is applied before remaining underscores in
the type name are encoded as `_u` (`Native_token` → `KioType_NativeToken`).
This includes the ordinary newtypes produced by `labels`;
the boundary rule depends on the lowered newtype declaration, not on which
surface spelling produced it. The FFI surfaces the declaration's member names.

```kio
// module m
pub newtype Counter : I32 { pub constructor fresh; pub projector current; };
```

```js
const c = pkg.m.KioType_Counter.fresh(0);     // constructor: I32 → Counter
const n = pkg.m.KioType_Counter.current(c);   // projector:   Counter → I32
```

Public newtype members use the same boundary representation as `pub fn`
parameters and returns. `fresh` accepts `shape(I32)` and returns
`shape(Counter)` (`{Counter: <i32>}`); `current` accepts that same
`shape(Counter)` and returns `shape(I32)`. A value built with
`pkg.m.KioType_Counter.fresh` can therefore be passed directly to any
`Counter`-typed public function, and a `Counter` returned by such a function
can be passed directly to `pkg.m.KioType_Counter.current`. Payload products, sums,
functions, polymorphic binders, and existential projector continuations use
the same recursive conversion rules as every other public signature.

Only public members are properties of the host namespace. If exactly one
member is public, the namespace exposes that member and the nominal uses the
hidden handle representation from § Structural and nominal types. A
constructor-only namespace can mint handles but cannot inspect them; a
projector-only namespace can inspect package-produced handles but cannot mint
them. If neither member is public, the namespace is empty and handles can only
be obtained from and shuttled through other public functions. This preserves
the declaration's operation visibility at the host boundary; a structural
payload object is used only when both operations are public.

## Higher-kinded types

JS is type-erased; the host runtime carries no type-system information at run-time, so Kio's HKT machinery has no representation overhead at the JS backend. The cross-cutting scheme is documented at [`backends/README.md` § Higher-kinded types](README.md#higher-kinded-types); the per-backend realization reduces to:

- A type-constructor application — `F(A)` for a kind-`*→*` binder, `Either(String)` for a partially-applied multi-arity newtype — is the same JS value as its carrier. No wrapper, no per-newtype marker; multi-arity type constructors render as ordinary nested applications.
- Lift/project are identity at every call site: a concrete type constructor's `mk_<ctor>` / `un_<proj>` and an abstract type constructor's threaded-instance `pure` / `extract` reduce to pass-through.

**Polymorphic newtype payloads.** The dictionary pattern (`pub newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B)`) needs no HKT-specific payload representation on JS. Its bound types erase, while the stored callable retains the application stages required by the cross-cutting contract in [`backends/README.md` § Polymorphic newtype payloads](README.md#polymorphic-newtype-payloads). Every binder of the stored first-class value is one hidden callable stage; a work-free stage returns the next callable, while a stage with computation performs it first. A direct known call may separately compact adjacent leading type applications. With both members public it uses the ordinary structural newtype boundary; hiding either member selects the same nominal handle rule as for every other newtype. Inside the body, construction and projection are identities around the complete staged callable; a public boundary wrapper performs the ordinary value-only function adaptation.

## Item naming

JS uses the [shared source-component word casing](README.md#source-derived-public-names):
`do_work` → `doWork`, `Native_token` → `NativeToken`, `_do_work_` → `_doWork_`.
An `<exact-component>` escapes remaining `_` as `_u` after word casing;
an `<exact-path>` additionally escapes `/` as `_s`.

Items appear on the host record (`host.<NS>.<name>`) and on the returned
package under their declaring module's namespace and their word-cased leaf.
For `<NS>`, a module path without source `_` replaces `/` with `_`;
a path containing `_` uses `KioModule_<exact-path>`. On the package surface,
the root module uses word casing, nested modules use
`KioModule_<exact-component>`, and public newtype handles use
`KioType_<exact-component>`. JS reserved words
(e.g. `class`, `delete`, `new`) are permitted because these names appear at
property-access positions, where JS allows reserved words.

## Worked example

Given a package `hello` with:

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

host type String role(str);
host fn print(s: String) -> .;
```

```kio
// hello/main.kio
module hello/main;

import hello(String, print);

pub fn main() -> . { print("hello\n"(String)) }
```

A Node host invokes the package as:

```js
import { createHello } from './out/js/hello.js';

const pkg = createHello({
  hello: {
    print: (s) => { process.stdout.write(s); return null; },
  },
});

pkg.hello.KioModule_main.main();
// → writes "hello\n" to stdout
```

### Structural values in action

A package that exposes a fn whose argument and return type are
structural products lets the host build and unpack values directly as
JS objects keyed by the right-spine slot:

```kio
// pair_swap.pkg.kio
package pair_swap;

bridge {
  pair_swap;
  pair_swap/**;
}
```

```kio
// pair_swap.kio
module pair_swap;

host type I32    role(i32);
host type String role(str);
```

```kio
// pair_swap/main.kio
module pair_swap/main;

import __intrinsics__;
import pair_swap(I32, String);

pub fn pair_swap(p: (I32 & String)) -> (String & I32) {
  __pair__(String, I32, __snd__(I32, String, p), __fst__(I32, String, p))
}
```

```js
import { createPairSwap } from './out/js/pair_swap.js';
const pkg = createPairSwap({});

const p = { _0: 42, _1: "hello" };               // construct an (I32 & String)
const q = pkg.pairSwap.KioModule_main.pairSwap(p); // (String & I32)
console.log(q._0, q._1);                         // "hello" 42
```

The host constructs and inspects values via the same key shape on
both sides of the boundary; the package's per-signature wrapper
converts between this JS shape and whatever internal rep codegen
chose, transparently.

A package with named labels surfaces them in the FFI shape too. Given the
following in a bridged module `hello/main`:

```kio
labels { greeting : String };

pub fn say_hello(g: Greeting) -> . { print(g.?{greeting}) }
```

```js
pkg.hello.KioModule_main.sayHello({ Greeting: "hello\n" }); // label-generated singleton
```

### Browser

A browser host loads it the same way:

```html
<script type="module">
  import { createHello } from './hello.js';
  const pkg = createHello({
    hello: {
      print: (s) => { document.body.append(s); return null; },
    },
  });
  pkg.hello.KioModule_main.main();
</script>
```

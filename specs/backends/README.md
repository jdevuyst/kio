# Backends

This directory holds the per-backend contracts for Kio's emitters: the
shape a host must call against to invoke a kio-built artifact. Each
backend page covers what's specific to one host language; the
cross-cutting properties common to every backend live in this file.

Every listed backend is presumed to implement the full Kio contract; listing a
backend does not create a target-specific feature tier. Backends carry no
independent version or maturity tier. Each backend instead publishes the
two-valued host-API compatibility status defined below. The normal backend page
and host guide have no quality or maturity banner; if a concrete known caveat
exists, both files carry matching top banners that name the actual breakage and
the host action it requires. Known broken behavior uses the same banner and
mutual-citation trail; publishing it discloses a fixable contract violation
rather than accepting a family divergence or claiming a proved-impossible
limitation.

## Backend pages

- [`js.md`](js.md) — JavaScript backend. Family `dynamic`.
- [`ts.md`](ts.md) — TypeScript backend. Family `typed-dynamic`. Typed-skin companion of the JS backend: the runtime `<ns>.js` is the JS backend's, byte-identical, plus a generated `<ns>.d.ts` natural-exact skin (`<ns>` the artifact namespace, defaulting to the package name).
- [`python.md`](python.md) — Python backend. Family `typed-dynamic`. A type-erased Python module with a namespace-shaped host record and package surface, plus a generated `.pyi` stub-package natural-exact skin.
- [`java.md`](java.md) — Java backend. Family `erased-static`. A statically-typed host with a typed FFI skin over an *erased* body (`Object`, the host's universal object); HKT is carried dynamically in that erased rep. The body is a serialized-IR interpreter rather than per-function compiled code — the page carries the family-divergence note.
- [`rust.md`](rust.md) — Rust backend. Family `erased-static`. A statically-typed host with a typed FFI skin over an *erased* body (an opaque reference-counted `Any` token: `Rc` by default, `Arc + Send + Sync` under the thread-safe option); HKT is carried dynamically in that erased rep. Reference-counted, not GC.
- [`go.md`](go.md) — Go backend. Family `erased-static`. A statically-typed host with a generic typed FFI facade over an *erased* body (`interface{}` / `[]any`); products use generic record shells and concrete sums use row-anchored `KioSum` values. HKT is carried dynamically in the erased rep.
- [`swift.md`](swift.md) — Swift backend. Family `erased-static`. A statically-typed host with an exact typed FFI skin over private `Any`-valued execution interfaces. Private callable and structural carriers retain their execution form inside the body; lexical type witnesses construct exact native views at the public boundary. Public sums use native enums with payloads.
- [`haskell.md`](haskell.md) — Haskell backend. Family `native-HKT`. A statically-typed host whose own kind system carries HKT directly (`F(A)` → `f a`); exact host types selected through a package-qualified marker class; products and sums as transparent pair / `Either` folds with flat boundary patterns; monad-polymorphic exports over a value host-function record. The first native-HKT member; its per-backend concern is forcing Kio's strict evaluation in a lazy host.

## Host API stability

Every host-language backend carries exactly one conspicuous field in both its
backend contract and matching host guide:

```text
**Host API stability:** `evolving`
```

Each field is followed by an adjacent link to this section; per-backend pages
publish their value and do not restate the shared policy.

The value token is exactly `evolving` or `stable`. The two values govern
compatibility only:

- **`evolving`** — a compatibility-breaking host-API change may be proposed,
  but the concrete break requires explicit user approval before implementation.
  The status is not standing authority for a break.
- **`stable`** — compatibility-breaking host-API changes are prohibited. The
  backend remains protected until an explicitly user-approved demotion to
  `evolving` is published before any otherwise-prohibited break.

This status is not a backend version, maturity scale, conformance grade, or
caveat. It says nothing about code-generation quality, correctness,
completeness, performance, or implementation readiness. Both values carry the
same complete Kio semantics, FFI laws, acceptance gates, and obligation to fix
bugs. Improvements that preserve compatibility are permitted at either value.

Compatibility is evaluated when Kio and the backend are upgraded while the Kio
package source, package and target configuration, and documented host contract
remain unchanged, with the same concrete generation baseline selected. A
`stable` backend lets conforming host source and integration
continue to compile after regeneration and preserves the documented API
behavior. The protected surface includes target and configuration keys, public
output and loading entry points, namespaces and handles, exposed names and
types, call stages, documented FFI representations and runtime interaction, and
the documented minimum host-language or runtime floor.

The status does not promise a binary ABI, compatibility for cached generated
artifacts, private generated helpers, or unspecified file layout. Intentional
package-contract evolution recorded by [`kio sig`](../versioning.md) is a
separate source of host changes and is not a backend compatibility break.

Every new host backend starts as `evolving`. Promotion and demotion both require
explicit user approval. Promotion establishes the then-documented host API as
the compatibility floor and is not retroactive. Backends that share an
emitter, runtime, or artifact may carry different values; a shared change must
preserve every `stable` backend that exposes it or split off a
compatibility-preserving layer.

For this policy, renaming a backend spec and its matching host guide is removal
of the old backend ID plus addition of the new ID; status does not transfer
across the rename. The old ID follows its removal rule, so a `stable` backend
is demoted in an earlier publication before it can be renamed. The new ID
starts as `evolving` like every other new backend.

[Kio′](../prime.md) is a compiler phase target, not a host-language backend,
and has no Host API stability value.

## Generation baselines

A host backend's `baseline` setting selects a supported generation contract:
its language/runtime requirements, required language modes and libraries, and
public host API. Each baseline implements the complete Kio contract. A concrete
baseline retains its compatibility guarantees when Kio or the development
toolchain is upgraded; increasing its floor or breaking its host API follows
the Host API stability policy above. A baseline is not an exact installed tool
version, generated-file checksum, private implementation layout, or binary ABI.

The floating selection `latest` follows the fixed supported-baseline table in
the selected Kio build. It can advance to a generation with different host
requirements or API, including for a backend marked `stable`; host code may
need adaptation when that selection advances. Omission and Unit float in the
same way until formatting materializes a concrete name. A concrete selection
does not advance automatically. The spellings and formatting rule are in
[package.md § Per-target keys](../package.md#per-target-keys).

Exact tools used to test a baseline are evidence for those environments. They
do not rename the baseline, raise its minimum requirement, or set a maximum
supported version where the backend contract admits newer versions. Fresh
output on an older toolchain, retained output on a newer toolchain, and
unchanged host source after regeneration establish different compatibility
properties.

## Language families

Backends are grouped into **families** by the constraints their host language imposes on emission. The family a backend belongs to fixes its shared semantic, body-model, and FFI-strategy idioms — the rules its per-backend page can reference instead of restate — while applicable host and boundary-occurrence capabilities select the IR-side planning, conversion, and runtime-support machinery the emitter consumes.

The taxonomy is normative: every per-backend page declares its family near the top. Family membership is observable from the host language's properties, not a stylistic call. The set is refinable — as new backends land and stress the boundaries, family rows are added or sub-families introduced.

| Family | Defining traits | HKT strategy | Members |
| --- | --- | --- | --- |
| `dynamic` | Runtime and public host surface erase types; polymorphism is trivial; closures are first-class; no generic header at the FFI; existential erasure is the default | **type-erased** (§ below) | JS |
| `typed-dynamic` | Runtime remains dynamically typed and type-erased, while a generated static declaration skin preserves exact declarations, constructor applications, callable stages, and existential relationships; closures remain first-class. The runtime may be the paired dynamic backend byte-for-byte (TypeScript/JS) or the backend's own dynamic module (Python). | **natural-exact skin** (§ below) over a type-erased runtime | TypeScript, Python |
| `erased-static` | Static types with an exact typed FFI skin over a genuine dynamic universal carrier (`interface{}` / `Any` / an opaque reference-counted Rust `Any` token / `Object`); GC or ARC / reference-counted; closures first-class; first-class enums / sealed types | **type-erased body, exact public skin** (§ below) — the dynamic carrier carries HKT privately while generated host types preserve each nameable public relation | Rust, Go, Swift, Java |
| `native-HKT` | Static types whose own system has higher-kinded + rank-N polymorphism; typed FFI skin; GC; closures first-class; the host may be lazy | **native-HKT** (§ below) — the host's kind system carries HKT directly | Haskell |

**The HKT body strategy is a family property** — how a host carries the
kind-`*→…→*` identity while executing Kio follows from the host language's type
system, so it is fixed by family, not chosen per backend. Public facade
precision is a separate capability rule: whenever the host can express an
exact nameable relationship, private body erasure does not authorize replacing
it with `any`, `Any`, `Object`, or an unchecked cast. The current combinations
are:

- **native-HKT** — the host's own type system has higher-kinded types, so Kio maps a kind-`*→*` type constructor directly to a host type constructor (`F(A)` → `f a`); no wrapper, no boundary erasure. **Haskell is the first member** (`RankNTypes` carries rank-N payloads; closed type families and patterns present structural values); the other static hosts whose type systems admit HKT and rank-N polymorphism (PureScript, Scala, Idris) would carry this same strategy. A native-HKT host that is *lazy* (Haskell) additionally forces Kio's strict evaluation in its body — the per-backend runtime concern, not an HKT one.
- **type-erased body** — a host whose universal value carries higher-kinded values. A dynamic host (JS) is type-erased throughout. A *static* host with a genuine dynamic universal carrier can erase the body to that carrier (Java's `Object`, Go's `interface{}` / `[]any`, Swift's `Any`, Rust's opaque reference-counted `Any` token). Type application does not itself traverse or rewrap an opaque value; ordinary nominal construction and projection still follow the backend's representation contract. Erasure does not prove that an arbitrary native facade application has the same type as the erased value, or eliminate the typed conversion at the facade boundary. The erased-static FFI restores every host-nameable exact constructor/application relation with the host's ordinary generic, marker, witness, or declaration-owned carrier mechanisms.
- **natural-exact skin** — the runtime remains type-erased, but a paired static
  declaration skin preserves each higher-kinded binder and application as an
  exact host type relation. TypeScript represents constructor witnesses with
  its structural `TypeLambda` protocol and computes applications with exact
  `Apply` / `Bind` helpers; this adds no runtime witness and does not change the
  byte-identical JavaScript body. Python's `.pyi` uses declaration-owned
  callable protocols and constructor/application witnesses over its
  type-erased dynamic module representation.

Native kinds, native rank-N payloads, static declaration skins, and body
erasure are independent axes. Erasure adds no runtime Kio type values or
host-supplied representation codecs to carry polymorphic type arguments.
Swift's native generic selectors are
specified in [§ 2](#2-type-erasure-at-the-ffi). Erasure does not prescribe
whether generated private adapters need representation evidence to construct
exact native values. Such evidence
provides no Kio typecase and cannot be recovered by inspecting opaque host
values. An exact skin preserves the public type relation, while a native-HKT
backend threads Kio's explicit dictionary values directly (Kio has no typeclasses — see
[§ Higher-kinded types](#higher-kinded-types)). See that section and
[§ Polymorphic newtype payloads](#polymorphic-newtype-payloads) for the
per-strategy detail.

A per-backend page **either** does what its family's shared idioms prescribe **or** carries an explicit, mutual-cited divergence in how its host realizes them. A family divergence remains permissible only while the backend implements the full Kio contract; it cannot excuse an unsupported or degraded Kio feature or serve as a per-backend limitation. A family gains a detail page under `specs/backends/families/<name>.md` when it has two or more shipping members and shared idioms start to repeat across per-backend pages; absent one, the per-backend pages reference cross-cutting properties from this README directly.

## Cross-cutting FFI conventions

The properties below apply to every Kio backend, regardless of
host language. They are stable language-level commitments, not
backend-specific obligations: a per-backend spec page may *show* how
its host language realizes a given property, but it cannot weaken or
contradict it. The list is therefore unversioned — when one of these
ever changes, that's a language-level decision, not a backend-level
one.

### 1. Synchronous, single-threaded, re-entrant calling convention

Calls in either direction are synchronous: a host that calls a package
entry blocks until the call returns, and package code that calls a
host fn blocks until the host returns. Kio has no concurrency primitives
at the language level; the package runs on whichever thread invoked
it, inheriting the host language's per-realm concurrency model.

Re-entrancy is permitted: a host fn invoked by the package may call
back into another export, and an export may call host fns
that themselves invoke other exports. Each backend picks the host
language's natural mechanism — JS uses synchronous function calls on
a single event-loop thread, and a typed backend with native threading
inherits the host language's threading model unchanged.

#### Tail calls within generated code

**Applies when:** a generated Kio invocation finishes by transferring to another
Kio callable through its documented callable representation, within one
artifact or between artifacts whose public types and callable support are
compatible.

A tail application supplies its enclosing invocation's result without further
computation. Result branches and the final body of a binding or sequence retain
that position; callees, arguments, conditions and constructed payloads do not.
A function body starts a new invocation when called, not when constructed.

Transferring to another Kio function in tail position does not retain a
generated activation merely to return its result.
This includes functions passed as arguments or carried through generic,
structural and nominal values, and final executing type-application stages.
The callee, earlier application stages and arguments still evaluate in their
specified order. Captures and package-instance ownership remain intact.

Passing a callable unchanged through host code or opaque host storage retains
this property. The host uses the callable type specified by its backend;
where that type is a wrapper, converting it explicitly to a native function
and passing the native function back does not recover the wrapper's transfer
capability. A host-authored function that calls a Kio callback is likewise a
new host computation, not unchanged transport. Compiler-generated adapters
must preserve the tail transfer and its required argument/result conversions.

This tail-call rule is not a bound on arbitrary host computation or explicit
native-function bridges. Calls with work remaining follow the rule below.
Neither rule makes incompatible public types interchangeable between artifacts.
Host runtimes and host-supplied sequencing operations retain their own
resource behavior.
Invocation depth is also distinct from the space occupied or reclaimed by
captured values.

#### Calls with work remaining

**Applies when:** generated Kio code calls another Kio callable through its
documented, compatible representation and must continue computing after that
call returns.

Pending guest computation uses managed storage: explicit saved continuations
or a growing host-runtime stack. A backend may retain one managed runtime frame
per pending call; this is not a constant-frame bound. Unlike a tail transfer,
each such call may retain its own pending work and live values, so space can
grow with depth. Available memory and host-runtime resource limits still apply.

Returning from the child resumes the caller once, with that child's result.
Callee evaluation, executing type-application stages, arguments, constructed
values and selected branches retain their specified evaluation order. A
returned function or polymorphic value is data; returning it does not execute
another application stage.

Unchanged transport of documented callables through host storage preserves
this behavior. A host-authored callback, an explicit native-function bridge,
or synchronous host re-entry starts a separate host computation; pending host
work is not captured. Host-supplied sequencing operations, including a host's
Monad implementation, retain their own operational and resource behavior.
That boundary does not reclassify generated pending computation as host work.
This rule also places no depth bound on destruction of arbitrary captured
value graphs.

### 2. Type erasure at the FFI

Polymorphic items — `[A]`-quantified exports, host fns with rank-N value
parameters, and function values flowing in either direction — do not add
runtime Kio type values or host-supplied representation codecs to carry their
type arguments across the FFI. A
`fn run[A](cfg: Config(A)) -> A` has only `cfg` as its Kio value argument.
A typed backend maps a direct declaration binder to its host language's
generic surface where one exists; JavaScript removes the type choice entirely.

Swift's generic surface includes the metatype selectors specified in
[`swift.md`](swift.md#host-record-contract): these select an already-declared
native generic parameter and are additional Swift arguments, not Kio value
parameters or representation codecs. A named method uses `asKioType<i>:` for
a generic absent from its native value parameters and result. A rank-N
carrier's `specialize(asKioType:)` selects one binder and returns the exact
residual value; selecting that type still advances the callable stage.

This is **representation erasure**, not evaluation-stage erasure. Inside the
package, each Kio `Forall` is still one ordered abstraction/application stage.
A direct, statically known declaration may compact work-free adjacent binders
when the host type system supports that representation. An escaped or computed
polymorphic value instead retains one uniform callable stage per `Forall`, so
same-typed values remain substitutable regardless of where their real work
occurs. A type-erased backend represents those stages with hidden calls. A
native-HKT backend may represent them with native type abstraction followed by
an action returning the next stage. Boundary adapters pure-lift work-free stages,
preserve computation at its actual stage, and introduce no runtime Kio type
value or host-supplied representation codec to carry type arguments in the
public ABI.
Haskell's concrete visible-type-application and sequencing spelling is
specified in [`haskell.md`](haskell.md).

### 3. FFI additivity

Adding **package-source** pub items to a bridged module's source tree — new
`pub fn`s, new newtypes — is strictly
additive at the FFI: existing host code that *calls* the package
keeps working without modification. Each backend specifies
how its emission preserves this property (in JS, by adding
properties to a returned object; a typed backend preserves it by
adding interface members or new files alongside existing ones).
This rule holds at either Host API stability value; `evolving` does not turn a
package-source addition into authority to rename or remove an existing entry.

This is the FFI corollary of Kio's behavioral additivity (§ 7
below): the language commits that the meaning of existing items
doesn't change as the package evolves, and the FFI commits that
artifacts are forward-compatible at the host's source level on
the **package-entry call side**. The module-body open-world property
(`specs/language.md` § Open-world design) is the language-level
guarantee inside a `*.kio` file; FFI additivity is the
corresponding cross-language guarantee at the boundary. The two
are distinct properties — open-world covers module-body
compilation, FFI additivity covers host-call-site
compatibility.

**Host-fn additions are not covered.** Adding a `host fn`
to a bridged module is a *contract-surface*
change, not an ordinary package-source addition: the host now has to
*supply* a new item. Existing package call sites still work, but the
package no longer initializes against the old host record — the host's
construction code must add the new item or construction fails (in
JS, the factory throws; see [`js.md`](js.md) § Host record
contract). This is a versioning event, governed by the
bridge contract (see
[`../package.md` § The bridge block](../package.md#the-bridge-block)),
not by FFI additivity. The two sides are distinct: export
additions are forward-compatible, host-fn additions are a
contract change.

### 4. Package isolation

Each instantiation of a package produces an independent instance.
Each instance retains its own host bindings and execution state; creating or
calling another instance does not replace them. Hosts can deliberately share
values and capabilities between instances. A received callback retains its
original captures and host bindings, while a function belonging to the
receiving instance uses that instance's bindings.

Instances of the same loaded compiled artifact can exchange nominal values
when their exact native host/type instantiations and value representations
are compatible. Instantiation does not create a fresh nominal declaration.
Declaration-owned private storage may therefore be shared by those instances;
constructor and projector visibility remains unchanged. Different declarations
or separately loaded artifacts are not identified merely because their names,
source, or payload shapes match. The host remains responsible for supplying
values at the declared types, including the native type arguments required by
its backend.

Multi-instance hosting is the default, not an opt-in. The symbol-level
coexistence of two artifacts in one host program is
[§ The package facade](#the-package-facade)'s guarantee.

### 5. Exception propagation

A host fn that throws propagates its exception unchanged: it
unwinds through the Kio call stack and emerges from the
originating host call site. The package wraps no host call.
Symmetrically, a package call that itself throws (most commonly the
runtime guard for an exhausted `!` value) unwinds through the host
call stack the same way.

The package commits to neither catching nor wrapping host
exceptions, and to surfacing its own runtime guards via the host
language's natural exception mechanism. Each backend picks its host
language's exception model and wires it through.

### 6. Well-foundedness inheritance

Backends can rely on Kio's totality: every Kio' value has finite wrap
depth and every Kio' computation terminates. Codegen does not need
defensive cycle-breaking when emitting recursive types or recursive
functions. Hosts can construct non-terminating computations through
host-supplied capabilities (a `fn` that runs an infinite
loop, for instance), but that's a host concern, not a property the
backend must defend against.

### 7. Behavioral additivity

Behavioral commitments visible across the FFI are stable and additive. As Kio
evolves, the language may surface new operations (intrinsics, structural shapes,
host-record idioms) at the FFI; it does not change the meaning of existing
ones. A backend API break may change how host source names or stages an
operation only under [§ Host API stability](#host-api-stability); it never
changes the Kio behavior that operation denotes or weakens any shared FFI law.
Absent such an explicitly approved `evolving` API break, each backend keeps its
emitted surface in step: existing conforming host code remains correct against
newer emitter versions of the same unchanged package, and only newly exposed
entries appear at the host's side.

### 8. Host-trait descriptor

The `host type` / `host fn` declarations in a package's bridged modules
commit the package to a backend-agnostic
host-side contract: every backend must surface the same items —
one item per declaration, no more — and a host implementing the
contract on one backend maps cleanly to the equivalent contract on
any other backend. The package's `bridge { … }` block decides which
modules' host items participate; the descriptor is derived from them.

**Namespace preservation.** Host items keep their declaring module's
namespace, so a `host fn print` in module `a` and one in module `b` are
distinct entries — the boundary is module-qualified, not flattened to
leaf names. A backend renders the namespaced boundary as a structural
hierarchy (nested records / sub-traits — rung 1) or as a
mangled-but-injective flat surface (`a__print` — rung 2); a naive-flat
rendering that collides on leaf names is forbidden. The identity retains the
ordered module components and their boundaries until backend rendering; a
source underscore and a module separator may not collapse to the same host
key. Since identity is preserved either way, the rung choice is a per-backend
rendering decision and owes no per-backend carve-out. The package's export
surface obeys the same ladder — see
[§ The package facade](#the-package-facade) § Typed access.

The contract has three layers:

- **Identity layer.** Every backend declares the same set of host
  items, in the same shape: one method per `host fn`, one
  declaration-keyed type entry per `host type`, each under its declaring
  module's namespace. Where the host facade exposes type bindings, those
  entries are separately addressable. The module-path-then-declaration order
  is the canonical order; backends preserve it where the host language honors
  order.
- **Syntax-admission layer.** A `host type X role(r);` declaration records
  which role-governed surface syntax may be used at `X`: the corresponding
  literals, or Boolean values and conditionals for `role(bool)`. A role does
  not choose, infer, or constrain the host-language representation of `X`.
  An annotation or expected type may select the exact declaration, but the
  literal's lexical role must still match that declaration's role. A roleless
  host type admits no lexical literal, and a mismatch is a type error before
  backend emission.
  Each backend separately specifies how the exact host-type declaration is
  represented or bound. The role never implies extra host functions or
  ambient runtime items.
- **Intent layer.** For each `host type`, the contract carries a
  set of *bound intents* — purposes the type's bound surface must
  discharge so the emitter's later uses (closure capture,
  type-erased storage, derived-shape equality) can rely on them.
  Each backend translates the intents to its own bound surface:
  Rust to `Clone + PartialEq + 'static`, while JS ignores the set
  entirely because it is dynamically typed.

The identity layer is declaration-keyed, not injective. Distinct opaque Kio
`host type` declarations remain distinct Kio types and retain separate
descriptor entries (and separate host binding entries where exposed), but a
host may bind any compatible entries to the same concrete host type or type
constructor. Backend emission must not infer a host-language nominal
distinction merely from the declarations' distinct Kio identities. Explicit
Kio `newtype` declarations are unaffected by this rule.

#### Declaration-keyed conversion adapters

Go, Java, and Python realize role
conversion as host-implemented members keyed by the exact `host type`
declaration. Go and Python share a readable-primary naming strategy: an
ordinarily spellable qualified identity uses a readable, reversible host name,
while a reserved exact encoding is the fallback for an ambiguous, unspellable,
or colliding identity. That selection is a pure function of the declaration
identity, never source order or namespace occupancy, so adding an unrelated
declaration cannot rename an existing adapter. Their pages specify the same
readable component join and exact fallback frame in their respective host
syntaxes. Java is the other current member of the capability cohort and uses
its exact declaration frame for every adapter, as specified on its backend
page.

### 9. Call-by-value evaluation order

Kio evaluates call-by-value ([`prime.md`](../prime.md) § Kio' semantics): an
application evaluates its callee expression exactly once. A type application
then consumes one `Forall` stage before evaluation advances; a value application
evaluates its value arguments exactly once from left to right and only then
invokes the call. Every backend must emit code that preserves this order. Type
erasure may remove the type's runtime payload, but never the abstraction or
application stage: a computed polymorphic value must complete its current type
stage before a following type stage or value argument is entered. A host
language whose application evaluation already follows the required value order
(JS, Rust, …) realizes that part directly and represents an erased executable
type-stage boundary as a hidden call. A backend whose host is lazy or has a
different or unspecified operand order must introduce explicit sequencing.
For example, Haskell output
uses monadic binds, `seq`, bang patterns, or an equivalent wherever a body
sequences host effects, so its artifact observes the same effect order as every
other backend.

### 10. Shared data stays unchanged

Host code must not modify Kio products, sums, or visible nominal payloads that
it shares with a package while the package can still use them. This includes
nested Kio values, whether passed directly, through a generic parameter, or
inside a host-managed container. The rule applies in both directions: to data
given to Kio and to data received from Kio, including callback arguments and
results. Returning from a call does not end this obligation if a retained
value or function can still reach the data. To change such data, the host
constructs a changed copy instead.

Reading a product field, a sum's selected alternative or payload, or a visible
nominal payload must behave like reading stored data: it yields the same value
without side effects. A stable, side-effect-free accessor is permitted; an
accessor that changes its result, updates state, or throws on a read is not.
This requirement concerns data fields, not invocation of a function value.

Opaque host types follow their own declared contracts. A host-provided mutable
`Box` may change through host functions such as `get` and `set`. A Kio tuple
containing that box keeps the same box in the same field; the box's
host-managed contents may change. The shared-data rule does not prohibit
stateful host capabilities or inspect opaque host objects to find their contents.
Replacing a box's stored value is allowed; modifying a Kio tuple that was
already shared remains forbidden even when the tuple is stored in that box.

Backends may reuse unchanged data rather than copy it. The boundary does not
promise fresh objects or independent snapshots, and hosts must not rely on
object identity being preserved or changed by a conversion. This does not
remove the backend's representation conversions, visibility guarantees, or
callable argument/result contracts.

### 11. Host values satisfy their declared types

The host is responsible for supplying values that match the declared types and
the backend's representations. This covers arguments passed to Kio, results
returned by host functions, and later callback arguments and results, including
values transported through generic parameters or opaque host containers.
Kio's static type guarantees assume that the host fulfills these obligations.

Generated packages do not perform general runtime type validation of host
values. Passing, returning, storing, or ignoring an invalid value need not
produce an error; a failure may occur only when the value is used, after other
host effects, or not at all. Hosts must not rely on a particular rejection
point or diagnostic for a violation of this obligation.

Representation conversions and the host language's required casts remain part
of the calling interface. Opaque nominal storage exposes a payload only through
the authorized operations and nominal ownership rules specified by that backend.
Such operations can reject an invalid value when accessing it; they do not imply a
recursive validation pass over its enclosing values. Host-record completeness
and precompiled-artifact loading have their separately specified checks.

## The package facade

Every emitted artifact exposes one uniform facade: a factory that takes
the host contract and returns the package handle. Like the numbered
conventions above, the rules here are stable language-level commitments —
a backend page shows how its host language realizes them, and cannot
weaken them.

### Typed access

A backend whose host language is statically typed surfaces the facade at
host types: the factory returns a nominal package-handle value whose
exports are reached through typed members, and the host contract is a
nominal host type the host implements. String-keyed lookup, untyped
universal-value handles, and host-side casts are not part of any
statically-typed backend's contract. A dynamically-typed backend surfaces
the same structure as its host language's natural namespace value
(attribute / property access over the export tree).

**Namespace preservation (export side).** Exported items keep their
declaring module's namespace exactly as host items do (§ 8): a backend
renders the export surface as a structural hierarchy (nested namespace
values or types — rung 1) or as a mangled-but-injective flat surface
(`a__f` — rung 2); a naive-flat rendering that collides on leaf
names is forbidden. As with the host record, the rung choice is a
per-backend rendering decision. The identity of each structural-hierarchy edge
includes both its exact source component and its selector role. Kio's value,
module, and type namespaces remain distinct even when a host language places
all three behind the same member-access syntax. A value `child`, nested module
`child`, and public newtype `Child` are a legal combination and must all remain
reachable, as must any other distinct selectors whose candidate host renderings
collide under the host language's identifier equivalence. Target rendering is
deterministic and injective under that equivalence; it cannot use
declaration-order or occupancy suffixes that rename an existing public selector
when an unrelated declaration is added.

### Public newtype capabilities

Every backend presents the same capability surface for a plain-public newtype
in a bridged module. The outer declaration contributes its nominal type. A
plain-public constructor contributes the typed operation from the payload to
that nominal type; a plain-public projector contributes the typed operation in
the other direction. A private or `pub(path)` member contributes no host
operation. Consequently the four surfaces are nominal-only, construct-only,
project-only, and construct-and-project according to the two member markers.

The outer and member visibilities compose: if the outer newtype is private or
`pub(path)`, none of its type or member surface reaches the host even when a
member is written `pub`. Conversely, a public nominal value may cross another
export without making its payload structural at the boundary. The payload
shape becomes part of the public facade only through an exported constructor,
projector, or some other public signature that names that shape independently.

This is a capability rule, not a prescription for a backend's internal value
layout. A backend may encode nominal values however its host language requires,
but it must not expose a private member as a convenience, omit a public member,
or require the host to use the payload representation for an opaque nominal
value. See [`../package.md` § The bridge block](../package.md#the-bridge-block)
for the contract-surface and type-closure rules.

### The package namespace

Package-owned symbols — the package-handle type, factory, host type, and nominal
types — live under one per-package namespace,
expressed in the host language's own namespacing construct (crate, module,
package declaration, or file-module). Runtime-support items and structural
facade declarations also live there unless they use one of the shared-support
mechanisms below. No backend
places a fixed-name executable or package-owned symbol at the host language's
global scope.

The namespace defaults to a per-backend derivation of the kio package
name — documented in each page's § Output layout — and every backend
accepts the same optional `namespace "<value>"` target key
([`../package.md` § Per-target keys](../package.md#per-target-keys)) to
override it. The default derivation is a pure function of the package
name (never of the target id). Artifact/import namespaces remain distinct
from public API word casing: JS, TS, Python, Java, Rust, and Go retain the
source package spelling unless a fixed host or support-name collision requires
escaping. Those defaults use `__kio_pkg_<escaped-source>`, with source `_`
encoded as `_u`. Swift and Haskell use the public title brand as their module
namespace, including the exact `KioPkg_...` frame for affixes and fixed
collisions. Each backend page names its reserved set and explicit namespace
grammar. The derivation preserves distinct valid source names and does not
consult other declarations. An explicit namespace is host configuration, not
a Kio source identifier; it follows its target's validation and the branding
rules below. Two packages with the same source name disambiguate with this key.

### Shared static type declarations

**Applies when:** generated type declarations are consumed only by the host's
static checker, without being imported or linked by executable code.

Package-independent structural type declarations may live in a shared support
namespace. All generated packages in one host build use one consistent support
implementation. Separately generated packages can refer to the same
declarations without requiring distinct copies of a generic type to be
interchangeable under the host checker's rules. This shares type descriptions,
not executable code, package state, or nominal identities.

Each artifact includes the complete support closure it needs. Generation and
ordinary host imports require no other generated package, external support
installation, or registry. Matching declaration identities have identical
definitions. Resolution must preserve both overlapping and disjoint
declarations in either package search order, and must not silently substitute
a conflicting definition.
Support namespaces cannot collide with configurable package namespaces.
Package-owned nominal types remain distinct, including when used as parameters
of shared structural types. Public package interfaces re-export the needed
types so host authors do not manage the shared support layout.

### Shared callable support

**Applies when:** compatible generated packages use nominal host types from a
common module to represent transferable callables.

The backend provides a shared support module containing those callable
types and their execution support. Each generated artifact includes the
support sources it requires. A host combining compatible artifacts builds and
links one common copy; no separately downloaded runtime is required. The
backend's loading instructions specify this step.

The module contains no package-instance state or ambient callback registry.
Callables retain their own captures and package-instance owners, and separate
invocations retain separate execution state, including during host reentry.
Package-owned nominal types remain distinct. Sharing a callable representation
does not make incompatible argument or result types interchangeable.

Packages linked against one support module use identical definitions and
calling contracts. Generated package names cannot collide with the support
namespace. Public callable interfaces
remain typed: host authors construct and invoke them without manipulating
erased execution state or supplying representation codecs.

### Source-derived public names

**Applies when:** a host-facing identifier contains a Kio source-name component.

Public word casing preserves the initial case and every leading/trailing
underscore, removes internal word-separating underscores, and capitalizes
each following word's initial: `do_work` → `doWork`, `Native_token` →
`NativeToken`, `_do_work_` → `_doWork_`. Title word casing additionally
capitalizes the first letter where host export or branding syntax requires it.
Apply this conversion to each semantic component separately, before inserting
fixed role prefixes, module/type/member separators, or indexed slot suffixes.
Those fixed components are not source words and are not re-cased.

A source-readable exact component applies word casing before escaping
remaining `_` as `_u`; an exact slash-path also escapes `/` as `_s`.
Source-readable length frames calculate lengths from the converted, escaped
component, not from the old source spelling. A frame whose format uses
unescaped components instead counts their converted UTF-8 bytes. Raw nominal
identity and opaque structural identity codecs, including their hashes, retain
their semantic input bytes; public presentation does not redefine type identity. Haskell's
raw exact fallback bytes likewise remain unchanged while its cosmetic suffix
uses word casing. Each backend's § Item naming specifies its host syntax and
role-framing realization.

Names within one artifact that become separate filesystem entries must also
remain distinct under case-insensitive filename comparison. For Java's
class-bearing nominal and facade type names, preserving source word boundaries
takes precedence over word casing; value and member selectors still use word
casing. See [Java item naming](java.md#item-naming).

Naming is declaration-local: the resolved semantic identity, naming role and
effective package namespace determine a public name, never declaration order
or the presence of another declaration. Host keyword escaping and exact
role-separated fallbacks preserve those distinctions. No old-spelling aliases
are added by the word-casing rule.

### Branded naming

The facade's public paths are rooted in the effective namespace, which defaults
to the Kio package name. Where a public name carries a brand, it derives from
that namespace's final segment. Here, a source-shaped component means
a valid user-declarable Kio value-name spelling, not a type-name spelling or an
arbitrary host identifier. On JS, TS, Python, Java, and Rust,
an ordinary source-shaped namespace component uses title word casing for its
brand (`my_pkg` → `MyPkg`). A source-shaped component with leading or trailing
underscores uses `KioPkg_` plus the title-word-cased component with remaining
`_` escaped as `_u` (`_my_pkg` → `KioPkg__uMyPkg`). A reserved default
`__kio_pkg_<escaped-source>` decodes its unmarked source component into the
same forced `KioPkg_` class (`__kio_pkg_class` → `KioPkg_Class`). Every other
explicit namespace component uses `KioNs_` followed by the lowercase
hexadecimal UTF-8 bytes (`MyPkg` → `KioNs_4d79506b67`). The classes are disjoint.
A leading `__` outside the canonical reserved-default frame belongs to this
explicit `KioNs_` domain, not the user-declarable source-name domain.
Java uses the final dotted segment; Rust uses its crate identifier after
Cargo's `-` → `_` mapping. Swift uses its exact effective module namespace as
the handle; Haskell uses its exact final module segment, without re-casing
an explicit override.

For the `create_` factory form, the value brand lowercases the initial of an
ordinary title brand, but preserves `KioPkg_` and `KioNs_` frames exactly.
The fixed `create_` prefix stays fixed: namespace `my_pkg` gives
`create_myPkg`, not `create_my_pkg` or `createMyPkg`, on Rust and Python.
For package `greeter` (default namespace `greeter`, or `Greeter` on
Swift/Haskell):

| Surface | Canonical name | Rendering |
| --- | --- | --- |
| Package handle type | Public namespace brand described above; fixed `Package` in Go | `Greeter`; `greeter.Package` in Go |
| Host contract type | The handle name + `Host`; fixed `Host` in Go | `GreeterHost`; `greeter.Host` in Go |
| Factory | Fixed host-language factory frame + public brand, or a namespace-qualified member | `create_greeter` (Rust, Python), `createGreeter` (JS, TS, Haskell), `greeter.New` (Go), `createGreeter(host:)` (Swift); where the factory is a static member of the handle type (Java), it is `create` |

Branding distinguishes selectively imported types and factories such as
`AlphaHost` / `BetaHost` and `createAlpha` / `createBeta`. Go instead uses
ordinary package qualification (`alpha.Host`, `beta.Host`, `alpha.New`) with
fixed role names; the package name need not be repeated in each member.
Namespace qualification
separates packages even when two dotted namespaces share a final segment and
therefore a brand. Overriding `namespace` on same-named packages separates
their namespace identities; changing the brand-bearing component also
separates their public brands. A dynamically-typed backend needs no named
handle or host type; its factory carries the brand.

### Coexistence

Two packages whose namespaces differ load into one host program without
symbol conflict. Packages combined in one build use one consistent support
implementation. Package-owned symbols remain namespace-qualified; shared static
declarations obey the identity and resolution rules above. Fixed member names
for package-local support types therefore remain separate, while shared static
definitions deliberately agree across packages. Neither arrangement merges
package-owned nominal identities or runtime state. Two packages with the same
name disambiguate with the `namespace` key. Instance-*state* isolation is § 4's
separate guarantee; this one is symbol-level.

### What this fixes and what stays free

Fixed here: the typed-access floor, the package-namespace rule and the
`namespace` key, the public facade names above, and coexistence. Still per-backend:
how the namespace construct is spelled, the rung choice for host record
and export surface, file layout within the applicable package or static-support
namespace, the host-record
construction idiom, atomic-type tables, and the encoding of sums,
products, and newtype values.

## Doc comments on the emitted surface

Documentation retained on a selected live boundary declaration is rendered in
the host's native documentation form: exported functions on the package facade,
host-function requirements, public newtype declarations, and host types where
the host presentation provides a declaration slot. A bare generic parameter
without such a slot does not acquire an artificial declaration for documentation.

Documentation is presentation-only. It does not change host obligations, public
names or types, callable behavior, effect order, or runtime-helper selection.
Private implementation and unbridged declarations do not publish their docs.
Module docs, docs discarded by declaration expansion, and declarations retained
only by signature history contribute no boundary docs. Newtype prose describes
the nominal declaration, not its constructor or projector. Signature files
continue to omit item documentation.

The shared input dialect is [Kiodoc](../kiodoc.md). Authored line breaks are
preserved without prose reflow. References render the written Kio name as code;
code-embedding directives render their operand as code without resolution,
source expansion, or host-language linking. Code and raw-HTML regions remain
literal regions. Emission is deterministic and best-effort: malformed markup
does not make a package build fail; validation remains `kio doc check`'s job.

Host-specific rendering and escaping prevent authored documentation from
closing its comment or string, introducing a native declaration annotation, or
selecting executable machinery. Native framing normalizes carriage-return
line endings or encodes control characters as string content; delimiters and
native directive markers can appear with visible escapes. Preserving prose
does not require copying unsafe source bytes into a host comment. Target pages
specify their dialect and attachment sites. Existing synthesized explanatory notes follow user prose,
separated by a blank documentation line.

## Structural FFI shape conventions

Beyond the behavioral rules above, the FFI also shares structural
conventions for how `&` (product) and `|` (sum) types map to the
host language. The *shape* of a structural value at the boundary
follows the source-level type, walked over the right spine; each
backend page describes what that shape *looks like* in its host
language (JS objects, Rust structs/enums, or another host-native
carrier). The walk is uniform. The key-naming algorithm applies when a
backend exposes keyed fields, cases, patterns, or presentation helpers; a
canonical positional carrier such as Rust's binary `Product` / `Sum` has no
keyed presentation to name.

### Generic substitution and opaque constructors

**Applies when:** a type parameter occurs in a host import, package export,
callback, returned function, or public nominal member.

Instantiating a generic boundary must preserve the same typed value relation as
the corresponding concrete boundary, including products, sums, nominal values
and callable stages nested under type constructors. A value supplied through
one path must remain usable through the other using the documented host
interface; separately usable but incompatible public and private shapes do not
satisfy this rule.

A host-selected constructor `F` is opaque unless its declared contract supplies
an operation. For example, substituting `T = Int` in `F(T & Bool)` cannot require
the backend to inspect or map `F` to convert its payload into `F(Int & Bool)`.
The same rule applies when the host constructs the payload and Kio later
receives it through a generic operation. Representations and typed adapters
must preserve this relation without assuming container operations, guessing
types from values, or weakening the
[shared-data rule](#10-shared-data-stays-unchanged). Adapters also
preserve each computation's [application stage](#2-type-erasure-at-the-ffi).

### Facade topology and execution provenance

**Applies when:** a backend renders a live `host fn`, exported function, or
public-newtype member, or preserves a removed `host fn`'s frozen boundary
declarations for env-side source stability.

For each such site, the exact current or frozen signature fixes the ordered
`Forall` / function stages and the reachable structural and nominal
dependencies. Its interface provenance independently says whether the site is
live or retained only for source stability. A backend preserves those facts
rather than reconstructing them from declaration spelling or generated names.

A retained site may keep the declarations and stable aliases its frozen
signature needs. Where [§ Deprecated host items](#deprecated-host-items) says
the backend re-emits an optional deprecated compatibility member, host source
may still name and directly call either its own supplied structural property or
the emitted trapping default without making either a current host obligation.
Retention never adds the item to loader
matching, live host dispatch, the package's exports or handle capabilities, or
an executable package body: package code has no path to invoke it. How a host
language declares generic shells, anchors a concrete sum row, or matches a sum
case remains a per-backend realization.

### Function-type FFI canonicalization

**Applies when:** a function crosses a package/host boundary, including host
functions, exports, callbacks, and public newtype members.

Internally Kio uses a strict System F arrow: a function's
`Type::Function` has one `param` and one `ret`. A declaration
signature with multiple value parameters right-folds its parameter
types to `Function(Product(A, Product(B, C)), R)`. Boundary planning records
two related facts once. The **semantic facade** walks each parameter's
right-spine: every `Product(L, R)` contributes the semantic slot for `L` and
continues into `R`, and the terminal contributes the final slot. The paired
**execution layout** freezes which slots came from each source value parameter
so the adapter can reconstruct the original runtime argument exactly once.

A directly written canonical Unit domain has no flattened semantic slots.
Its native declaration view is nullary; a uniform first-class callable
representation may instead require an explicit Unit value for that function
layer, as specified by the backend. Substitution does not erase an existing
value parameter: if `A -> R` is instantiated with `A = .`, that parameter
remains one explicit Unit value. The semantic domain, source parameter groups
and native calling interface remain separate facts.

The native argument list preserves those source-parameter partitions. Thus
`fn first(x: Int, y: Int) -> R` takes two native arguments, whereas
`fn second(pair: Int & Int) -> R` takes one native product argument, despite
their equal normalized Kio function types and semantic slot plans. A callback
whose written domain is one product likewise receives that product as one
argument. Structural fields inside the product follow the backend's documented
representation; they are not extra native call arguments. Where a native
arity limit requires a containing shell, the backend page specifies that
realization. The adapter reconstructs the exact internal product and applies
each source stage exactly once. This host presentation does not change Kio
call inference, semantic signature equality, or the dynamic loader's canonical
descriptor groups.

An exported fn's host surface additionally compacts **across declaration-head
value groups** into one facade call. The boundary wrapper applies the internal
curried layers one group at a time, so the host never sees the intermediate
closures. Type-binder groups remain in their declared positions while the
wrapper performs that internal application, but they add no host value
parameters; the wrapper supplies or consumes their required internal stages
before advancing to the next value group. A curried function *value* crossing
the boundary (a closure parameter or return) stays curried:
each `Function(Pᵢ, Rᵢ)` layer maps to one call, with intervening `Forall`
stages adapted without adding a value argument. The semantic walk is a
product-theoretic rule rather than a printer convention: exported value groups
are finite products at the Kio level, tuple values are elements of those
products, and Kio uses the right-associated binary tree as the definitional
representation. Local function values and elaborator glue keep their product
type; only a prepared host-boundary facade receives the semantic slot plan and
paired execution layout. See
[`specs/language.md` § Type parameters](../language.md#type-parameters)
for the right-fold rule and the per-backend page for the
language-specific instantiation.

### The right-spine walk

A `Type::Product(A, B)` whose right child is itself a product
extends the spine with `A` at the current index and continues
into the right child; otherwise the spine ends with the right
child as its last slot. The same rule applies to sums.
The walk yields one ordered semantic slot plan for a right-associative chain;
an applicable spine-keyed presentation may expose that plan as one flat n-keyed
shape, while the backend's signature carrier may remain recursively binary.
Left-associative chains preserve their nesting in either realization:

- `(A & B)` — 2-slot product, slots `[A, B]`.
- `(A & B & C)` = `(A & (B & C))` — 3-slot product, slots `[A, B, C]`.
- `((A & B) & C)` — 2-slot product, slots `[(A & B), C]`. The
  first slot is itself a 2-slot product.
- `(A | B | C)` = `(A | (B | C))` — 3-slot sum, slots `[A, B, C]`.

When the backend exposes a spine-keyed presentation, each spine slot contributes one
key — populated for products (every key set), single-key for sums (exactly one
key set, identifying which slot the value inhabits). Other carriers preserve the
same type grouping without exposing spine-slot keys; a recursively binary
record's fixed child keys are not semantic spine-slot keys.

The walk is **shared between FFI canonicalization and the
spine-based elaborator palette** (a user-defined coercion library;
see [`../../docs/poc/elab.md`](../../docs/poc/elab.md) § The spine
palette). The
FFI side derives its slot plan from the walk and, for spine-keyed presentations,
derives the per-backend keys there; the spine palette
matches source and target slot-by-slot under the same walk, with
no algebraic normalization. The two sides reason about a value's
shape with one mental model: the user-written grouping (left- vs
right-associative) survives end to end.

### 3-step key fallback

**Applies when:** a host presentation exposes structural spine slots as keys.

Each spine slot's key comes from the first non-colliding
candidate of:

1. **Bare newtype name.** When the slot's type resolves to a
   `newtype F`, propose `F` as the key. The rule applies to every
   newtype. For a type generated by `labels { f : X };`, the key is
   the generated type name `F`, not the member name `get`. Named public
   fields and variants render source components with the shared
   [word casing](#source-derived-public-names), followed by their host-specific
   key syntax. The semantic key and opaque structural identity remain raw.
2. **Canonical fully-qualified spelling.** If step 1 collides
   with a previously-claimed slot key, propose `<modulepath>.<F>`
   — the declaring module's path joined to the unqualified type name.
3. **Positional `_<n>`.** Fall back to the slot's 0-based index
   on the spine.

First non-colliding candidate wins. Slots whose type does not
resolve to a newtype skip steps 1 and 2 (no newtype name) and
start at step 3.

The user-site syntax — `{f = x}` vs. qualified `{m.f = x}` vs.
direct `F.mk(x)` — does not change the proposal: every form reaches
the same canonical newtype, and step 1 uses the newtype's type name.

### Generic structural shell identity

Every prepared product or sum shell has one backend-neutral identity:
its structural kind plus its ordered 3-step semantic keys. Concrete payload
types, nested payload topology, source spans, and declaration order are not
part of that identity. They remain generic arguments at each shell use. Two
sites with the same kind and semantic keys therefore share one shell without
sharing or erasing their payload types, while adding an unrelated declaration
cannot rename either site.

Where a backend materializes the shell, this identity selects its stable
host-facing declaration. The backend page states whether that declaration is a
signature carrier, an alias or construction/matching helper, or unnecessary
for that host representation. The identity itself does not choose among those
realizations.

The common anonymous binary positional shells are `Product` and `Sum` on hosts
that materialize names. When a backend materializes a named non-anonymous
shell, the shared reversible readable codec is:
`KioFacade_<Kind>_K<arity>_...`. Each key frame records its role and exact
content: `B` plus one escaped component for a bare name, `Q` plus all framed
module components and the leaf for a qualified name, or `P<index>_` for a
positional key. Component frames carry their escaped byte length; underscores
and non-alphanumeric UTF-8 bytes are escaped, so concatenation is injective.

A backend may instead use the shared reversible exact-frame spelling
`KioFacadeX_<hex>` when its identifier syntax cannot carry the readable form.
A host with a stricter fixed path-component budget may use a bounded digest of
the complete encoded identity, but that choice is a pure function of the
identity and target limit, never declaration order or namespace occupancy. It
must reject any digest collision between distinct identities. The backend page
states that host-specific spelling and limit.

Some hosts do not need a named semantic shell: JavaScript and
TypeScript use matching object shapes, Python may use structural protocols,
Rust uses canonical binary `Product` / `Sum` plus exact per-boundary aliases,
Haskell uses its native families and stable boundary aliases, and Go uses its
generic product and row-anchored sum realization. Every rendering consumes the
prepared payload topology and site identity. Only a rendering with keyed
presentation or access consumes the ordered semantic keys; absence of a minted
type name does not authorize a second shape walk.

### Per-backend obligations

Each backend page describes how its host language realizes the
above. The variation lives in:

- **Key syntax**: how a slot's key is rendered against the
  host language's identifier grammar. Backends whose grammar
  forbids `.` in identifiers must mangle the
  step-2 spelling; backends whose grammar permits arbitrary
  string keys (JS object keys via bracket access) can use the
  unmangled form.
- **Public-name rendering**: how the shared
  [source-component word casing](#source-derived-public-names) combines with
  host export capitalization, keyword escapes, and exact role frames.
  Per-backend pages specify their realization under § Item naming.
- **Shape carrier**: what host-language construct carries the
  shape. JS uses plain objects; Rust uses structs and enums.
- **Per-slot accessor**: how a host reads or constructs a slot.
  JS uses dot/bracket access; Rust uses field access or enum
  matching depending on the shape.
- **Shell spelling and target limits**: whether the host materializes the
  shared generic shell, how it renders the reversible identity, and whether a
  fixed target-path limit requires a bounded exact-identity digest. Per-backend
  pages spell this out under § FFI surface / § Structural and nominal types.

A backend page references this section by name when describing
its instantiation, rather than restating the algorithm.

## Runtime-support library

Some backends emit a fixed support-library file alongside the
per-package source files, holding helpers the per-variant emit
calls into instead of inlining the bodies. The convention is
opt-in: a backend may keep the helpers inline in its ordinary output (JS
native fn values, Python dynamic typing, Haskell's private facade
declarations), manage a namespace-varying support file itself (Java), or ship
a fixed file when its representation and output layout call for one (Rust's
opaque `KioStoredValue`, marker-directed facade conversion, and typed generic
dispatch helpers). The per-backend output layout states which realization
applies.

The [shared callable support](#shared-callable-support) form instead uses one
module common to compatible artifacts. Its documented public callable
types are part of the host API; its generated execution linkage remains
implementation support. The module's complete source travels with each artifact.

For package-local support files, two properties hold:

- **Canonical content, namespaced location.** The file's content is
  canonical per emitter version and documented representation-affecting target
  options *up to the package-namespace binding* —
  the namespace-bearing header line (a `package` clause, a `module`
  header) the emitter substitutes; below that binding the content is
  byte-identical across packages built with those same options, and the kio
  emitter is the source of truth (build-side code writes it; package-semantic
  variation beyond the binding would defeat the convention). The file lives under the
  package's namespace — inside the emitted crate, package, or module
  tree — never at a fixed shared path, so two packages' support files
  cannot collide.
- **Not user-modifiable.** The emitter overwrites the file on
  every build. Host code must not import from it as a stable
  public API — its names and shape are internal to the
  package's emit, fall outside Host API stability, and may change between Kio
  versions.

The per-backend page documents the file's path (e.g. Rust:
`src/__kio_runtime.rs`) under § Output layout, alongside the
package's main source files. Backends that need no support file
omit the row.

This convention does **not** affect the "no external
dependencies" property each backend page maintains: the file is
internal to the emitted artifact, not a separate dep the host
must install.

## Deprecated host items

[§ 3 FFI additivity](#3-ffi-additivity) and [§ 7 Behavioral
additivity](#7-behavioral-additivity) cover the **addition** side of a
package's evolution: existing host call sites keep working as the package
gains exports. Deprecated host items are the **removal** side of the same
story, and they apply to the *host-implementation* surface rather than the
host-call surface.

Removing a `host fn` is a compatible env-side change at the language level
(see [`../versioning.md` § The compatibility
relation](../versioning.md#the-compatibility-relation)): an existing host
already supplies a superset of what the new package requires, so the package
still loads. But the regenerated host-side interface (a Rust `Host` trait, …)
no longer declares the removed item, while the host's hand-written
implementation still does — so unchanged host source may stop compiling. This
is **source stability**, a backend-level property distinct from the language
compatibility relation (see [`../versioning.md` § Two relations the design keeps
distinct](../versioning.md#two-relations-the-design-keeps-distinct)).

The shared idiom: a backend may **re-emit a removed host item in a deprecated,
optional form** so existing host source keeps compiling across the removal. A
new host supplies and selects only live items; retained history never adds a
method, type choice, generic binding, factory argument, or other obligation.
The signature
comes from the changelog history — the item's introducing `add` / `modify`
block (see [`../versioning.md` § Deprecated host
items](../versioning.md#deprecated-host-items)) — so the changelog is a build
input for any backend that does this. Properties common to every backend that
re-emits:

- **It is codegen-only.** A re-emitted removed item is invisible to the
  loader's structural match; it exists purely to keep host source compiling.
  The package never calls it, so its re-emitted body (if any) is unreachable
  from package code.
- **Every history-only declaration is deprecated.** A callable shim and every
  host-visible binding, carrier, alias, shell, or support type emitted solely
  for its frozen signature use the target's stable deprecation mechanism. An
  exact declaration that is also live follows live provenance and is not
  deprecated.
- **It is optional for a current host.** A structural compatibility member may
  be genuinely omittable; a defaulted shim must compile when a new host omits
  it. If the target cannot preserve old source without forcing the new host to
  implement or select a removed item, the backend emits no shim and carries a
  concrete source-compatibility caveat instead. A source edit for the old host
  is preferable to a dead supply obligation for every new host.
- **The deprecation window is emitter policy.** *How long* a backend keeps
  re-emitting a removed item is the emitter's decision, read from the `remove`
  / `add` history. The changelog itself performs no window bookkeeping (see
  [`../versioning.md` § History growth and `compact`](../versioning.md#history-growth-and-compact)).
- **The contravariant asymmetry still holds.** Re-emit addresses *removal*
  source-stability only. Adding a host item remains a breaking env change on
  every backend — the host must supply the new item or construction fails.

A removal-tolerant structural host may need no callable member, or its typed
artifact may keep one as a genuinely omittable deprecated property when that
preserves contextual source typing. It may also keep the exact carrier, shell,
alias, or type-binding declarations reachable from the removed function's
frozen signature. These are source-compatibility declarations, not live host
items: they are explicitly deprecated, add no package capability, loader
match, dispatch entry, or body adapter, and cannot force a current host to
provide or select a removed item.

A backend whose host language makes a given removal genuinely source-stable
re-emits nothing for it (a removal-tolerant dynamic host, for instance). A
backend that *cannot* keep a particular removal source-stable — where the host
language has no construct that preserves the removed surface without a host
source edit — states that concrete caveat in matching top banners in its
backend page and host guide, and mutual-cites the detailed limitation between
the backend page and emitter code (the limitation paragraph on the backend page
and a code comment at the degradation site naming that paragraph). Each backend
page documents its realization under a § Deprecated host items section:

- **JS** ([`js.md`](js.md#deprecated-host-items)) — removal-tolerant; the host
  record is a plain object, so a missing property is a no-op. Nothing is
  re-emitted, and incompatible history epochs have no generated source impact.
- **TypeScript** ([`ts.md`](ts.md#deprecated-host-items)) — the byte-identical
  JS runtime stays current-only, while the `.d.ts` retains each representable
  removed host function as an exact-signature optional `readonly` property
  with recognized `@deprecated` JSDoc. Every emitted history-only transitive
  type, binding, and helper is likewise deprecated and imposes no current
  selection; live provenance remains required and nondeprecated. A retained root whose
  closure conflicts with another declaration epoch is omitted under
  [§ Incompatible retained declaration epochs](#incompatible-retained-declaration-epochs).
- **Python** ([`python.md`](python.md#deprecated-host-items)) — the runtime host
  record is removal-tolerant and ignores an extra old field. Python 3.10's
  standard library has no recognized stub deprecation marker, so the `.pyi`
  and `.py` omit every history-only root, adapter, carrier, method, and support
  declaration. Existing source that names one of those generated declarations
  must remove or replace that reference, as the backend's concrete caveat
  records; incompatible retained epochs are already covered by that complete
  omission.
- **Java** ([`java.md`](java.md#deprecated-host-items)) — re-emits a removed
  `host fn` as a `@Deprecated` `default` method with a throwing body on the
  host interface — the Java analogue of Rust's deprecated diverging default —
  so a host still `@Override`-ing the removed item keeps compiling when its
  frozen signature has no removed host-type dependency or incompatible
  nominal epoch. A removed `host type`
  is excluded from package-root generics; a deprecated
  declaration-owned carrier and deprecated throwing adapter defaults keep a
  frozen method nameable without imposing a current selection. Old source
  drops any explicit argument for the removed root and deletes or rewrites
  dependent method and adapter overrides, as the backend's concrete caveat
  records. Every retained-only support declaration is `@Deprecated`.
  Incompatible retained epochs omit every affected method and type, requiring
  the corresponding override or generated-type reference to be edited.
- **Rust** ([`rust.md`](rust.md#deprecated-host-items)) — re-emits a removed
  `host fn` whose signature remains nameable as a `#[deprecated]` trait method
  with a diverging default body. A removed nullary host facade associated type
  or parameterized-host `Storage` associated type, and any
  retained method or support declaration that depends on it, is omitted so a
  current host supplies no history; existing source must delete those stale
  trait-implementation items. Other history-only support is `#[deprecated]`.
  An incompatible retained epoch has the same source impact for every affected
  method and type reference.
- **Go** ([`go.md`](go.md#deprecated-host-items)) — removal-tolerant at the
  method boundary: structural interface satisfaction lets a host retain the
  removed method while satisfying the smaller `Host` interface, so no removed
  method is re-emitted. A representable frozen signature's stable `Env_`
  aliases and complete reachable type closure remain available with standard
  `// Deprecated:` directives. A removed nullary host type is a deprecated
  nominal rather than a current package-root parameter, so old source must
  drop that generic selection as the backend's concrete caveat records.
  Incompatible retained epochs omit the affected aliases and types, so an old
  extra method signature or direct type reference may require an edit.
- **Swift** ([`swift.md`](swift.md#deprecated-host-items)) — removal-tolerant;
  re-emits a removed host function as an `@available`-deprecated protocol
  requirement with an equally deprecated trapping extension default. Removed
  nullary host types retained by a representable frozen signature receive
  deprecated concrete defaults, and every emitted history-only transitive declaration is deprecated;
  current hosts select and implement only live items, and no retained item
  enters package dispatch. Incompatible retained epochs omit the affected
  requirement and type closure, so an old extra method or generated-type
  reference may require an edit.
- **Haskell** ([`haskell.md`](haskell.md#deprecated-host-items)) — re-emits no
  removed host-function field, and documents `host fn` removal as a genuine
  source-stability limitation: the `Host h m` record has no per-field default,
  so a removed field cannot be kept settable-but-optional. A removed `host
  type` with a compatible retained epoch remains as a `DEPRECATED`, privately defaulted compatibility-only
  associated type so an old equation warns and a current instance omits it;
  it contributes no record field, adapter, capability, or execution path.
  Incompatible retained type epochs omit the affected compatibility equations
  and type closure, requiring old equations or generated-type references to be
  edited.

### Incompatible retained declaration epochs

One host-visible declaration identity cannot denote two incompatible exact
declarations in one generated artifact. When a live declaration and a frozen
epoch disagree at the same qualified nominal identity, the live declaration
wins and every retained root that depends on the incompatible epoch is
omitted. When two retained epochs disagree, every retained root that depends
on that identity is omitted; choosing either epoch would make the result
history-order-dependent and falsify the other frozen signature. No union,
intersection, erased carrier, or reused spelling is an exact replacement for
both declarations.

This omission never changes current-host requirements or package execution,
but typed host source that names an affected compatibility method or generated
type must delete or rewrite that reference. JavaScript has no such generated
history surface. Python already omits all history-only declarations. On
TypeScript, Java, Rust, Go, Swift, and Haskell, the backend page and host guide
state the concrete source edit in their matching caveat banners.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs), whose
comment cites this subsection. Its backend-specific source-impact fan-out is
[TypeScript](ts.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-typescript),
[Go](go.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-go),
[Java](java.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-java),
[Rust](rust.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-rust),
[Swift](swift.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-swift),
and
[Haskell](haskell.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-haskell).

## Higher-kinded types

Kio expresses higher-kinded types through kind-annotated binders and direct type application (`F(A)`, `Either(String)`; see [`prime.md` § Higher-kinded types](../prime.md#higher-kinded-types)). How a backend carries the kind-`*→…→*` identity through to host code follows from its family's **HKT strategy** (§ Language families):

- **native-HKT** backends map a kind-`*→*` type constructor straight onto a host type constructor (`F(A)` → `f a`). No value wrapper, no boundary erasure: the host's own kind system carries the identity.
- **type-erased** backends carry values through the universal execution interface (`Object` / `interface{}` / `Any` / Rust's opaque reference-counted token). Type application `F(A)` introduces no implicit traversal or rewrapping of an opaque value. Nominal constructors and projectors retain their backend-defined carriers and operations; erasure does not make all such operations runtime identities. Nor does it imply native facade equality or eliminate typed boundary conversion. The callable that carries a polymorphic operation retains its application stages (§ Polymorphic newtype payloads below). The dictionary that underwrites a `Functor` / `Monad` instance is an ordinary value the body builds and applies; a static backend's FFI surface preserves the exact marker, application carrier, and conversion relation its host can name.

**Erasure handles the private HKT body with no HKT-specific walk.** A
type-erased backend — including a static host that erases its body to a dynamic
universal carrier — uses one private representation for values carried through
concrete and abstract constructor applications. Its typed facade still owns
the exact conversion between that representation and each public application
carrier. Body identity therefore cannot be cited as proof of native generic
associated-type equality or as permission to omit the public marker/application
relation.

What every backend keeps, regardless of strategy, is **explicit dictionary threading**: Kio has no typeclasses, and instance coherence is local to the `derive!` call tuple (see [`language.md` § The `derive!` elaborator](../language.md#the-derive-elaborator)), incompatible with a host's global instance coherence. So a `Functor` / `Monad` instance is an ordinary value Kio passes as a `fn` parameter — never a host typeclass instance — on every backend. A native-HKT backend adds native kinds + native rank-N payloads on top; a type-erased backend needs neither.

Each backend page documents its own realization under a § Higher-kinded types section:

- **JS** ([`js.md`](js.md)) — type-erased; type application does not rewrap values. Nominal construction and projection preserve the complete nominal carrier, including when it is transported through an opaque host constructor; callable payloads retain their stages.
- **TypeScript** ([`ts.md`](ts.md#higher-kinded-types)) — natural-exact skin over a type-erased runtime; the runtime body is the JS backend's `.js` verbatim, so `F(A)` carries no additional runtime representation, while the `.d.ts` preserves the exact constructor/application relation through its structural `TypeLambda`, `Apply`, and `Bind` protocol.
- **Python** ([`python.md`](python.md#higher-kinded-types)) — natural-exact skin over a type-erased runtime; type application does not rewrap values, while the typed-stub package preserves its declaration-owned constructor and application relation. Nominal construction and projection follow the declared carrier and visibility contract, and a polymorphic newtype payload retains its staged callable shape.
- **Java** ([`java.md`](java.md#higher-kinded-types)) — the private body uses `Object`; the exact public skin names each declaration with a constructor marker, records `F(A)` through its typed application witness, and owns the conversion to that carrier. Private body identity does not replace the public witness relation.
- **Rust** ([`rust.md`](rust.md#higher-kinded-types)) — the body is type-erased; the exact public skin represents every saturated value type with a `KioType` marker and `Facade`, represents an abstract constructor application with `KioAppliedN<F, ...>` / `KioApplyN<F, ...>`, and converts explicitly through the opaque stored representation. Rust does not require or claim native `F::Apply` equality.
- **Go** ([`go.md`](go.md#higher-kinded-types)) — the private body uses `any`; the public skin uses exact kind-constrained type indices, `Prepared[I]`, free slot/declaration boundary conversion functions, declaration-owned application carriers, and typed selection helpers for first-class `TypeStage` values. Selection preserves each application stage and its captured host conversion context without traversing opaque host storage.
- **Swift** ([`swift.md`](swift.md)) — the private body stores callable and structural execution carriers as `Any`; the exact public skin uses declaration-owned constructor markers and unary application carriers. Private lexical type witnesses construct native boundary views without replacing the internal callable stages; opaque host-owned payloads are neither traversed nor reindexed.
- **Haskell** ([`haskell.md`](haskell.md#body-model)) — native-HKT; `F(A)` maps to `f a`. Direct known declarations may use consecutive native `forall`s, while escaped polymorphic values have one internal monadic stage per binder.

## Polymorphic newtype payloads

A newtype payload may itself be a polymorphic function — the dictionary pattern that underwrites `Functor[*F]`, `Monad[*F]`, and similar instance-style abstractions:

```kio
pub newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B) {
  pub constructor mk_monad;  pub projector bind;  };
```

The payload's outermost shape is a unary chain such as
`Forall(A, Forall(B, Function(…)))`. Construction supplies any value of that
polymorphic function type; projection yields a value the call site uses at
concrete type arguments (often inferred from a `do`-block step).

Each backend page describes its concrete realization:

- **type-erased** ([`js.md`](js.md), [`ts.md`](ts.md#higher-kinded-types), [`python.md`](python.md#higher-kinded-types), [`java.md`](java.md#higher-kinded-types), [`rust.md`](rust.md#higher-kinded-types), [`go.md`](go.md), [`swift.md`](swift.md)) — abstract carriers erase to the body's universal value, but the payload remains an ordinary staged callable. Every binder is one hidden callable stage; a work-free stage returns the next callable, while a stage with computation performs it before that return. A direct statically known call may separately compact adjacent leading type applications. Construction and projection preserve those stages and use the backend's ordinary nominal and application-carrier conversions; private body identity does not authorize omitting the typed facade seam. TypeScript uses the JavaScript runtime realization.
- **native-HKT** — stores the polymorphic field at its rank-N type directly (e.g. Haskell `RankNTypes`). Its internal first-class callable representation retains one stage per binder; on Haskell each stage returns the next value through the host-selected monad. An adapter may pure-lift a work-free stage but does not remove it. A known declaration may separately keep consecutive binders compact in its direct scheme. Construction and projection use the backend's ordinary newtype representation and do not regroup binders. The dictionary itself is still an ordinary value Kio threads explicitly, not a host typeclass instance.

A backend that ships HKT but lacks polymorphic-newtype-payload support is non-conformant. A claim that the gap is genuinely impossible requires matching concrete-caveat banners in `specs/backends/<lang>.md` and `docs/hosts/<lang>.md`, plus an emitter comment at the exact degradation site citing the detailed spec section; it is never a family divergence.

## What stays per-backend

Each backend page covers what genuinely varies by host language:

- The family declaration, any concrete known-caveat banner mirrored in the
  host guide, the required Host API stability field, the host-language version
  floor, and the output layout (one file vs. many, file naming under the package
  namespace). Backends carry no independent version or maturity tier.
- How the loading protocol is realized — sync mechanics and the
  host-record construction idiom. The factory and type *names*, the
  namespace scheme, and the typed-access floor are fixed by
  [§ The package facade](#the-package-facade).
- How each exact host-type declaration is represented or host-bound. A
  `role(...)` admits surface syntax; it never selects that representation.
- The encoding choice for sums, products, and newtype values
  (host-native records, tagged enums, opaque handles, etc.).
- Item-naming and identifier-mangling rules — each host language
  has its own reserved-word set.
- The worked example.

A backend page references this file by section number when restating
or instantiating a cross-cutting property; it does not redefine one.

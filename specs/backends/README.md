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
- [`swift.md`](swift.md) — Swift backend. Family `erased-static`. A statically-typed host with a typed FFI skin over an *erased* body (`Any` / `[Any]`); HKT is carried dynamically in that erased rep. The skin uses native `enum`-with-payload sums — the family's native-sum member.
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
remain unchanged. A `stable` backend lets conforming host source and integration
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
- **type-erased body** — a host whose universal value carries HKT with no private representation distinct from its carrier, so body lift / peel are identity. A dynamic host (JS) is type-erased throughout. A *static* host with a genuine dynamic universal carrier can erase the body to that carrier (Java's `Object`, Go's `interface{}` / `[]any`, Swift's `Any`, Rust's opaque reference-counted `Any` token). That private fact removes any HKT-specific body walk; it does not prove that an arbitrary native facade application has the same type as the erased value, or eliminate the typed conversion at the facade boundary. The erased-static FFI restores every host-nameable exact constructor/application relation with the host's ordinary generic, marker, witness, or declaration-owned carrier mechanisms.
- **natural-exact skin** — the runtime remains type-erased, but a paired static
  declaration skin preserves each higher-kinded binder and application as an
  exact host type relation. TypeScript represents constructor witnesses with
  its structural `TypeLambda` protocol and computes applications with exact
  `Apply` / `Bind` helpers; this adds no runtime witness and does not change the
  byte-identical JavaScript body. Python's `.pyi` uses declaration-owned
  callable protocols and constructor/application witnesses over its
  type-erased dynamic module representation.

Native kinds, native rank-N payloads, static declaration skins, and body
erasure are independent axes: a type-erased runtime needs no private
per-host-kind machinery, an exact skin preserves the public type relation
without changing that runtime, and a native-HKT backend threads Kio's explicit
dictionary values directly (Kio has no typeclasses — see
[§ Higher-kinded types](#higher-kinded-types)). See that section and
[§ Polymorphic newtype payloads](#polymorphic-newtype-payloads) for the
per-strategy detail.

Additional families are added here when a backend page that needs them lands. Conformance audits read this table at run time, so adding a family is a README edit, not an audit-code change.

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

### 2. Type erasure at the FFI

Polymorphic items — `[A]`-quantified exports, host fns with rank-N value
parameters, and function values flowing in either direction — do not add
term-level type arguments to the FFI. A `fn run[A](cfg: Config(A)) -> A`
still exposes only `cfg` as a runtime argument. A typed backend maps a direct
declaration binder to its host language's generic surface where one exists;
an erased backend removes the type choice entirely.

This is **representation erasure**, not evaluation-stage erasure. Inside the
package, each Kio `Forall` is still one ordered abstraction/application stage.
A direct, statically known declaration may compact work-free adjacent binders
when the host type system supports that representation. An escaped or computed
polymorphic value instead retains one uniform callable stage per `Forall`, so
same-typed values remain substitutable regardless of where their real work
occurs. A type-erased backend represents those stages with hidden calls. A
native-HKT backend may represent them with native type abstraction followed by
an action returning the next stage. Boundary adapters pure-lift work-free stages,
preserve computation at its actual stage, and expose no term-level type
argument in the public ABI.
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
Two instances — whether created from the same emitted artifact or
from different packages — share no state, no caches, no host-record
references. Multi-instance hosting is the default, not an opt-in: a
host running multiple Kio packages does so without interference
between them. Isolation covers instance *state*; the symbol-level
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

The same descriptor — `host_types` / `host_fns` (each carrying its
declaring module's path) plus the per-host-type bound-intent set —
drives every backend's host-trait emission. The descriptor is built by
scanning the bridged modules' `pub host` items. Adding a new backend adds
a renderer that consumes the descriptor; it does not add a module scan of
its own.

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

Every top-level symbol an artifact contributes to a host program — the
package-handle type, the factory, the host type, structural facade symbols,
and the runtime-support items — lives under one per-package namespace,
expressed in the host language's own namespacing construct (crate,
module, package declaration, or file-module). No backend places a
fixed-name symbol at the host language's global scope.

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

Naming is declaration-local: the resolved semantic identity, naming role and
effective package namespace determine a public name, never declaration order
or the presence of another declaration. Host keyword escaping and exact
role-separated fallbacks preserve those distinctions. No old-spelling aliases
are added by the word-casing rule.

### Branded naming

The facade's public names derive from the effective namespace — its final
segment, for a dotted namespace — which itself defaults to the kio package
name, so one knob roots every emitted name and a consumer can derive the
whole surface from the artifact alone. Here, a source-shaped component means
a valid user-declarable Kio value-name spelling, not a type-name spelling or an
arbitrary host identifier. On JS, TS, Python, Java, Rust, and Go,
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
| Package handle type | Public namespace brand described above | `Greeter` |
| Host contract type | the handle name + `Host` | `GreeterHost` |
| Factory | Fixed host-language factory frame + public brand | `create_greeter` (Rust, Python), `createGreeter` (JS, TS, Haskell), `CreateGreeter` (Go), `createGreeter(host:)` (Swift); where the factory is a static member of the handle type (Java), it is `create` |

Branding is what makes multi-package hosting ergonomic: two packages'
types and factories (`AlphaHost` / `BetaHost`, `createAlpha` /
`createBeta`) carry recognizable package brands. Namespace qualification
separates packages even when two dotted namespaces share a final segment and
therefore a brand. Overriding `namespace` on same-named packages separates
their namespace identities; changing the brand-bearing component also
separates their public brands. A dynamically-typed backend needs no named
handle or host type; its factory carries the brand.

### Coexistence

Two packages whose namespaces differ load into one host program without
symbol conflict — including artifacts emitted by different kio versions.
The guarantee follows from the namespace rule's totality: every artifact
symbol is either the namespace itself or is scoped beneath it. A backend may
therefore use fixed member names for canonical support types without creating
an unqualified host-program collision; the distinct package / module / crate
identity still separates them. Two packages with the same name disambiguate
with the `namespace` key. Instance-*state* isolation is § 4's separate
guarantee; this one is symbol-level.

### What this fixes and what stays free

Fixed here: the typed-access floor, the package-namespace rule and the
`namespace` key, the branded names, and coexistence. Still per-backend:
how the namespace construct is spelled, the rung choice for host record
and export surface, file layout inside the namespace, the host-record
construction idiom, atomic-type tables, and the encoding of sums,
products, and newtype values.

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

Internally Kio uses a strict System F arrow: a function's
`Type::Function` has one `param` and one `ret`. A declaration
signature with multiple value parameters right-folds its parameter
types to `Function(Product(A, Product(B, C)), R)`. Boundary planning records
two related facts once. The **semantic facade** walks each parameter's
right-spine: every `Product(L, R)` contributes the semantic slot for `L` and
continues into `R`, and the terminal contributes the final slot. The paired
**execution layout** freezes which slots came from each source value parameter
so the adapter can reconstruct the original runtime argument exactly once.

A directly written canonical Unit domain has no semantic slots and is
nullary. Substitution does not erase an existing value parameter: if `A -> R`
is instantiated with `A = .`, that parameter remains one explicit Unit value.
This semantic distinction is recorded before a backend chooses host syntax.

A positional backend may expose one host argument per semantic slot. A backend
whose natural public shell groups a product instead exposes one object for that
source parameter, with the same right-spine semantic keys. Consequently
`fn f(x: Int, y: Int) -> R` and `fn f(x: Int & Int) -> R` have the same
right-spine semantic slots but distinct source-parameter partitions; a backend
may render those partitions differently without recomputing their topology.
In both realizations the execution adapter rebuilds the exact internal product
and applies the source body once.

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
an applicable keyed presentation may expose that plan as one flat n-keyed
shape, while the backend's signature carrier may remain recursively binary.
Left-associative chains preserve their nesting in either realization:

- `(A & B)` — 2-slot product, slots `[A, B]`.
- `(A & B & C)` = `(A & (B & C))` — 3-slot product, slots `[A, B, C]`.
- `((A & B) & C)` — 2-slot product, slots `[(A & B), C]`. The
  first slot is itself a 2-slot product.
- `(A | B | C)` = `(A | (B | C))` — 3-slot sum, slots `[A, B, C]`.

When the backend exposes a keyed presentation, each spine slot contributes one
key — populated for products (every key set), single-key for sums (exactly one
key set, identifying which slot the value inhabits). A positional-only carrier
still consumes the ordered slot plan but has no semantic key to expose.

The walk is **shared between FFI canonicalization and the
spine-based elaborator palette** (a user-defined coercion library;
see [`../../docs/poc/elab.md`](../../docs/poc/elab.md) § The spine
palette). The
FFI side derives its slot plan from the walk and, for keyed presentations,
derives the per-backend keys there; the spine palette
matches source and target slot-by-slot under the same walk, with
no algebraic normalization. The two sides reason about a value's
shape with one mental model: the user-written grouping (left- vs
right-associative) survives end to end.

### 3-step key fallback

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
`KioFacade_V1_<Kind>_K<arity>_...`. Each key frame records its role and exact
content: `B` plus one escaped component for a bare name, `Q` plus all framed
module components and the leaf for a qualified name, or `P<index>_` for a
positional key. Component frames carry their escaped byte length; underscores
and non-alphanumeric UTF-8 bytes are escaped, so concatenation is injective.
The codec is versioned spelling compatibility, not semantic identity.

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

Two properties hold for every backend that ships one:

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
- **type-erased** backends leave the private body representation unchanged across higher-kinded application; `F(A)` adds no HKT-specific body representation to the universal value (`Object` / `interface{}` / `Any` / Rust's opaque reference-counted token). Lift / peel are identities inside that erased body. This says neither that an arbitrary native facade type can be recovered by equality from the universal value nor that the typed boundary needs no conversion. It also says nothing about flattening the callable that carries a polymorphic operation: its application stages follow § Polymorphic newtype payloads below. The dictionary that underwrites a `Functor` / `Monad` instance is an ordinary erased value the body builds and applies; a static backend's FFI surface re-imposes the exact marker, application carrier, and conversion relation its host can name.

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

- **JS** ([`js.md`](js.md)) — type-erased; a type-constructor application carries no runtime representation distinct from its carrier, and lift / peel are identity at every call site. Multi-arity type constructors render as ordinary nested applications with no extra machinery.
- **TypeScript** ([`ts.md`](ts.md#higher-kinded-types)) — natural-exact skin over a type-erased runtime; the runtime body is the JS backend's `.js` verbatim, so `F(A)` carries no additional runtime representation, while the `.d.ts` preserves the exact constructor/application relation through its structural `TypeLambda`, `Apply`, and `Bind` protocol.
- **Python** ([`python.md`](python.md#higher-kinded-types)) — natural-exact skin over a type-erased runtime; a type-constructor application has no separate Python value representation, while the typed-stub package preserves its declaration-owned constructor and application relation. Lift / project remain identity in the erased body, and a polymorphic newtype payload retains its staged callable shape.
- **Java** ([`java.md`](java.md#higher-kinded-types)) — the private body uses `Object`; the exact public skin names each declaration with a constructor marker, records `F(A)` through its typed application witness, and owns the conversion to that carrier. Private body identity does not replace the public witness relation.
- **Rust** ([`rust.md`](rust.md#higher-kinded-types)) — the body is type-erased; the exact public skin represents every saturated value type with a `KioType` marker and `Facade`, represents an abstract constructor application with `KioAppliedN<F, ...>` / `KioApplyN<F, ...>`, and converts explicitly through the opaque stored representation. Rust does not require or claim native `F::Apply` equality.
- **Go** ([`go.md`](go.md)) — a free function-local type binder erases to `any`; exact parameterized declarations instead use their declaration-owned generic carrier and adapter at the public boundary. Go therefore does not assert a native generic-constructor equality.
- **Swift** ([`swift.md`](swift.md)) — the private body uses uniform `Any`; the exact public skin uses declaration-owned constructor markers, `KioApplyN` carriers, and their typed conversion seam.
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

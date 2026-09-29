# Haskell backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `native-HKT` (see
[`README.md` § Language families](README.md#language-families)).

> **Known host-source compatibility caveat.** Removing a bridged `host fn`
> changes the generated Haskell record, so an existing host must remove that
> field from its `<Handle>Host { … }` literal. See
> [§ Host-fn removal is not source-stable on Haskell](#host-fn-removal-is-not-source-stable-on-haskell).
> When retained type declarations require incompatible epochs of one exact
> nominal identity, Haskell also omits the affected compatibility equations
> and type closure; existing host source must delete or rewrite those equations
> and generated-type references. See
> [§ Incompatible retained declaration epochs are not source-stable on Haskell](#incompatible-retained-declaration-epochs-are-not-source-stable-on-haskell).

The `"haskell"` build target ([`specs/package.md` § Build target
files](../package.md#build-target-files)) emits a self-contained Haskell
module. This page describes the surface a Haskell host calls against.

Haskell is the first **`native-HKT`** family member: a statically typed
host whose own type system expresses higher-kinded and rank-N
polymorphism, so Kio's kind-`*→*` carriers map onto Haskell type
constructors directly (`F(A)` → `f a`) with no carrier brand and no
carrier-walk (see [`README.md` § Higher-kinded types](README.md#higher-kinded-types)).
The package does have a host-authored marker type `h`, but that marker
selects the package's exact `host type` bindings; it does not brand or
erase HKT carriers. The genuinely hard host-language difference is
**strict evaluation in a lazy host** (§ Boundary semantics, strict
evaluation).

The package's exported surface is polymorphic in both the host-type
marker `h` and the host monad `m`. Every emitted function is constrained
by `Monad m`; the host supplies a **value** record of `m`-returning host
functions and separately supplies a marker-class instance selecting the
Haskell representation of every Kio `host type`. The host commonly uses
`IO` for `m`, or a pure/test monad when appropriate. An all-pure
rendering is a non-goal — the host boundary is monadic.

## Language version

Emitted code targets **GHC 9.10+** with the `text` package for generated string
values. The gating features are ordinary GHC source extensions supported by
that floor. Generated modules enable the extensions they use with `LANGUAGE`
pragmas:

- `RankNTypes` — rank-N value payloads cross at their `forall` type
  directly (no erasure), with each first-class type-application stage
  returning its next stage through `m`.
- `ExistentialQuantification` — an existential Kio newtype retains its
  hidden payload binders in a native Haskell carrier.
- `ImpredicativeTypes` — rank-N payloads remain usable inside ordinary
  product, sum, and type-application positions.
- `TypeFamilies` / `KindSignatures` / `ExplicitNamespaces` — the marker class carries one
  kind-correct associated type per exact Kio `host type` declaration,
  while the package's structural product and sum families fold type-level
  slot lists to native Haskell carriers. `ExplicitNamespaces` lets a
  history-only associated family carry the native
  `{-# DEPRECATED type … #-}` marker without ambiguity.
- `DataKinds` / `TypeOperators` — structural slot lists use promoted lists
  and `':`.
- `PatternSynonyms` — stable boundary aliases expose flat bidirectional
  product and sum patterns over those carriers.
- `TypeAbstractions` / `TypeApplications` — generated native bodies use
  explicit one-binder type abstractions and applications for rank-N values and
  exact impredicative bindings. A known declaration may keep adjacent binders
  as consecutive native `forall`s. An escaped polymorphic value instead uses
  one monadic result stage per `forall`, so every first-class value of the same
  Kio type has one uniform callable representation.
- `ViewPatterns` — generated, non-exported view helpers preserve rank-N product
  and sum slots behind the ordinary flat pattern surface.
- `PolyKinds` plus per-instance overlap pragmas — a private
  generated superclass constraint rejects an instance that omits any live
  host-type equation, at that equation's exact kind.
- `ConstraintKinds` — the private host-type completeness superclass is
  assembled as a constraint synonym.
- `PackageImports` — references to standard modules select their supplying
  `base` or `text` package explicitly. Generated imports bind those modules to
  namespace-derived, strict-descendant aliases, so an otherwise valid package
  namespace may have the same spelling as a standard module without creating
  a self-import or an ambiguous qualifier.
- `FlexibleContexts` — ordinary Haskell constraints record what each
  module function's literals and conditionals require of the exact
  host-selected types it uses.
- `BangPatterns` / strict data fields — Kio's strict evaluation forces
  bound values in Haskell's lazy host (§ Boundary semantics).
- `ScopedTypeVariables` — for the boundary wrappers' local annotations.

A host module that constructs or matches a structural boundary alias enables
`PatternSynonyms`. If that alias contains a rank-N slot, it also enables
`ImpredicativeTypes` (and `RankNTypes` to write the slot type). Ordinary use of
the exported patterns needs neither `TypeApplications` nor
`AllowAmbiguousTypes`; any type applications used to implement a generated
rank-N view remain inside the emitted package module. A host module that
explicitly selects a first-class polymorphic value's type with visible
application such as `value @Bool` enables `TypeApplications`.

Memory is **GC** — automatic, so the body threads no ownership annotations.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target haskell {
    out "out/haskell/";
  }
}
```

`kio build haskell` writes one self-contained Haskell module under `out` for
the host to `import`. The module lives under the package's **namespace**
`<Ns>`: the Kio package name title-word-cased into
a GHC module segment by default (`csv_reconcile` → `CsvReconcile`), which
the `[A-Z]…` module grammar the `module <Ns>` clause requires. A
derivation landing on `Main` (which would collide with the host program's
own `Main`) or `Prelude` (which would shadow the implicit `Prelude`) gets
the exact `KioPkg_` frame (`main` → `KioPkg_Main`, `prelude` →
`KioPkg_Prelude`), so the default is always cleanly importable. Source names
with leading or trailing underscores use that frame too, with remaining `_`
escaped as `_u` after title word casing (`_my_pkg` → `KioPkg__uMyPkg`). The `namespace`
build-block key ([`specs/package.md`
§ Per-target keys](../package.md#per-target-keys)) overrides it: an
explicit value is a dotted GHC module path (`Com.Acme.Greeter`) whose
segments each match `[A-Z][A-Za-z0-9_']*` and whose whole is neither
`Main` nor `Prelude`; anything else is a build error. The facade's branded
names all derive from the namespace's **exact final segment**, without
re-casing an explicit override: for `<pkg>` =
`greeter`, the handle is `Greeter`, the marker class is
`GreeterHostTypes`, the host-function record is `GreeterHost`, and the
factory is `createGreeter`.

GHC's module-name = path rule maps a dotted namespace to directories, so
`Com.Acme.Greeter` writes `Com/Acme/Greeter.hs`. For a single-segment `<Ns>`:

- `<Ns>.hs` — the module's invocable surface: the
  `<Handle>HostTypes h` marker class, the `<Handle>Host h m`
  host-function record, the namespace-branded structural families (normally
  `<Handle>Product` and `<Handle>Sum`), stable boundary aliases and patterns,
  the `<Handle> h m` handle, the `create<Handle>` factory, and every exported
  boundary wrapper.
  Implementation functions and namespace-derived runtime support are also
  declared in this module but are not exported. **The entry point and complete
  artifact;** rewritten on every build, not user-modifiable.

The host authored the package's manifest, so it knows the namespace
without reading build output; the derivation above makes every branded
name reconstructible from the `module` clause alone. Every emitted declaration
is scoped beneath its distinct `<Ns>` module, and private runtime declarations
additionally derive their spelling from `<Ns>`; therefore two kio packages
with distinct namespaces coexist in one GHC program. The FFI surface (the
`<Handle>HostTypes` class, `<Handle>Host` record, structural families and
boundary aliases/patterns, `create<Handle>` factory, and exported wrappers) is exported from
`<Ns>`; the body's internal functions are not the contract.

## Loading protocol

Per [`README.md`](README.md) (synchronous, in-process). A Haskell host
(for a package `greeter`):

1. imports the package module, conventionally qualified
   (`import qualified Greeter`), and imports the members of
   `GreeterHostTypes` where it defines associated-type equations;
2. declares a marker type `h` and an instance
   `Greeter.GreeterHostTypes h` that selects a Haskell type for every Kio
   `host type` (§ Host record contract);
3. builds a `Greeter.GreeterHost h m` value — a record of `m`-returning
   functions, one per `host fn` — at its chosen monad `m`;
4. calls `Greeter.createGreeter` on it to obtain a
   `Greeter.Greeter h m`;
5. invokes exported items through the package's exported wrapper
   functions (§ Package API).

Package instantiation has no runtime failure mode at the boundary: a
missing or ill-kinded host-type equation, an incompatible direct-syntax
constraint, or a structural mismatch in `<Handle>Host h m` is a GHC
**compile error** when the host module is built. A missing live equation is
rejected at the marker instance even when no package function uses that
type; the error names the exact Kio `module/type`. The statically typed
interface is the contract.

## Package API

Exported items (`pub fn` in a bridged module) are reached through
**top-level boundary-wrapper functions** on the package module `<Ns>` —
a multi-value-group export's wrapper takes every group's parameters
flat in one call and regroups them onto the internal layers
([`README.md` § Function-type FFI canonicalization](README.md#function-type-ffi-canonicalization)). A
`pub fn` named `fn` in module `a/b` is the wrapper
`export__a__b__fn`, taking
the `<Handle> h m` handle and the function's value parameters at their
boundary shapes, returning `m <ret>`. Hosts copy this semantic name from the
emitted module; § Item naming defines its readable primary and exact fallback.

An exported wrapper's signature is the host's typed contract: each value
parameter is its boundary shape (§ FFI surface), and the return is its
boundary shape (`()` for a `()` return). The wrapper runs the package
function while threading effects through `m`; any body representation is
internal and never replaces a concrete host type or native boundary shape
in the wrapper's public signature.

Every exported `pub newtype` contributes one declaration-specific host
surface, even when it exports no member wrappers. It exposes exactly the
constructor and projector whose declarations are `pub`:

| Public members | Haskell boundary value |
| --- | --- |
| neither | an abstract nominal carrier with no member wrappers |
| constructor only | the carrier plus only its constructor wrapper |
| projector only | the carrier plus only its projector wrapper |
| both | the backend's established transparent-or-nominal representation |

For the both-member case, a nullary, non-recursive, non-existential newtype
is transparent: both wrappers use the declared payload's exact boundary
type. Every parametric or recursive newtype instead has one
declaration-stable abstract Haskell type constructor, including
dictionary-shaped and ordinary function-payload newtypes. Opaque and
one-member newtypes use such a carrier regardless of payload shape, so the
unexported operation cannot be recovered from raw representation access. The
same nominal head appears in every boundary signature, whether unsaturated
or fully applied; substituting its parameters never changes its identity.

An exported **existential newtype** is carried by a native Haskell
existential carrier. Its constructor quantifies the universal and hidden
binders, accepts the exact payload boundary shape, and seals the hidden
types in the carrier. Its projector quantifies the universals and result
type outside, then accepts a rank-N continuation. Each hidden binder is a
first-class application stage: a one-slot payload with one hidden binder uses
`forall hidden. m (payload -> m r)`, while canonical Unit uses
`forall hidden. m (m r)`. Further hidden binders nest another
`forall hidden. m (...)` stage apiece. The hidden binders never escape in the
result type. These are public continuation schemes: native type application
supplies no term-level type argument. The carrier type is exported abstractly;
its raw Haskell data
constructor and internal unpacker are not exported, so only the Kio members
whose own visibility is `pub` grant construction or elimination authority.
Each selected member retains its exact Kio scheme in the Haskell boundary
signature.

Carrier identity is the exact declaring module plus newtype name. Two
same-leaf declarations in different modules remain different Haskell types.
A carrier is an atomic boundary leaf: its hidden payload is not walked while
deriving an enclosing shape. Thus a both-member `Outer` whose payload is an
opaque `Hidden` contains the `Hidden` carrier rather than reopening its
payload, including when the declarations refer to one another.

There is no separate generic intrinsic API for constructing opaque
structural values: a structural value crosses the boundary as its
native pair / `Either` carrier, named at each boundary slot by a stable alias.
Exported flat pattern synonyms let the host build and read that value without
depending on its nested carrier spelling. See § FFI surface.

## Host record contract

The Haskell host contract has two deliberately separate parts:

- `<Handle>HostTypes h` is a marker class with associated types. It says
  what each exact Kio `host type` means in this host.
- `<Handle>Host h m` is a value record containing only host functions.
  It says how the package performs each `host fn` in monad `m`.

The marker `h` has no required value or representation. A host normally
declares an empty data type and gives it an instance:

```haskell
data AppHostTypes

instance Greeter.GreeterHostTypes AppHostTypes where
  type HostType__greeter__Str AppHostTypes = String
```

Every `host type` declaration in a bridged module contributes one
associated type to the package-qualified class, whether or not it has a
role and whether or not a live host function happens to mention it. The
associated family identifies the declaration by its full module path and
leaf; two modules declaring the same leaf get different families. Its primary
spelling is `HostType__<module path>__<leaf>` when every source component is
boundary-safe (§ Item naming). An unsafe or ambiguous route, or a primary that
overlaps a fixed facade type, uses the exact fallback defined there.

A parameterized, necessarily roleless Kio declaration becomes a higher-kinded associated
family. The marker is the only family argument on the left-hand side;
the Kio parameters occur in the result kind. For example,
`host type Array[T];` contributes an associated family shaped like
`type HostType__…__Array h :: Type -> Type`, and a use `Array(A)` becomes
`HostType__…__Array h a`. Every Kio host-type parameter has kind `*`, so every
corresponding Haskell argument has kind `Type`.

Every live associated family has a private, kind-correct sentinel default.
That default is not a representation choice: a private, poly-kinded class has
a universal success instance and one more-specific `TypeError` instance per
sentinel. The universal instance's local incoherence admits a present selection
that is a type variable or an unreduced host family; this is sound because a
host module cannot name the unexported sentinel, while an omitted equation has
already reduced to that sentinel and selects its error instance. Consequently
an instance must define every live equation even when the corresponding Kio
type is otherwise unused, without depending on `-Wmissing-methods` or
warning-as-error flags. The sentinel and completeness-class names and the
superclass constraint are not exported; the instances require no extra host
declaration.

A Kio role never selects that associated type's Haskell representation.
`role(i32)` admits the corresponding Kio literal syntax; it does not mean
`Data.Int.Int32`. The host may select `Integer`, a fixed-width type, or a
domain type. When emitted Haskell uses a literal or conditional directly,
ordinary Haskell constraints validate the selected type: integral,
fractional, and string literals induce `Num`, `Fractional`, and
`Data.String.IsString` respectively, while a Boolean literal or a value
used as a Haskell conditional condition induces equality with `Bool`.
Each generated module-function signature carries only the constraints induced
by literal and conditional syntax in that function or in a top-level function
it transitively reaches. Calls, partial applications, and first-class
top-level function references all contribute an edge; syntax inside a closure
constrains the enclosing function because the generated Haskell lambda captures
the dictionary. An exported wrapper carries the same constraints as the module
function it invokes. The package factory carries only `<Handle>HostTypes h`
and `Monad m`, so an unrelated function cannot constrain package creation or
another export. Thus a role-bearing type used only at host boundaries need not
be one of Haskell's built-in scalar types.

Each `host fn` declared in a bridged module becomes one field on the
`<Handle>Host h m` record in `<Ns>.hs`. The host supplies a value whose
fields are its implementations.

A field uses the public naming rule in § Item naming. For example,
`host fn print` in module `app/io` is
`host__app__io__print`. The `host` prefix makes the result a legal lowercase
record field. An unsafe route uses the exact fallback; a host normally copies
that exceptional spelling from the emitted `<Handle>Host` declaration.

Each field's type is `<arg shapes…> -> m <ret shape>` — the value
parameters at their boundary shapes (§ FFI surface), curried, returning
the monad applied to the return's boundary shape. The argument list is the
shared callable facade's canonical slot list at every value stage: a
right-spine product domain contributes its positional slots, while a direct
Unit domain contributes no term argument. A slot whose already-established
type is later instantiated with Unit remains one `()` argument. The Haskell
adapter uses the paired execution layout to rebuild the declaration's original
source parameters before invoking the body, so the public slot cut never
depends on a backend-local signature walk. A concrete host type is
spelled by its exact associated family applied to `h`; a generic host fn
quantifies its Kio type parameters with native Haskell `forall`, without
adding term-level type-argument slots to the call ABI. Adjacent binders in
this public field scheme remain adjacent quantifiers, and the field executes
only when its declared value call reaches the body. If package code treats
the field as a first-class polymorphic value, each binder becomes an internal
`m` stage; those adapter stages are not public field parameters. A nested,
first-class Kio type `[A] B` is
`forall a. m B`: visible type application selects `a`, then the host
sequences that action to obtain the next value or application stage. Direct
declaration binders remain the outer `forall` on the field itself. A `()`
return is `m ()`. The host
record is a **value** of monad-returning functions, not the marker-class
instance: Kio dictionaries remain Kio values and are not translated into
Haskell typeclasses. Plain data shapes carry no `m`; function results and
first-class type-application stages do.

Structural shapes under generic binders remain fully typed. A product or
sum that mentions scoped parameters adds those parameters to its stable
`Env_H…` alias in lexical binder order, preserving both repeated occurrences
and binder kinds. The alias expands through `<Handle>Product` or
`<Handle>Sum`; substituting a parameter changes only the family arguments,
never the representation rule. Thus substituting `T := B & C` in `A & T`
yields the same Haskell type as translating `A & (B & C)` directly. An
abstract application such as `F(A & B)` is therefore
`f (Env_H… h m a b)` on both sides of the call; the backend does not attempt
to traverse an arbitrary host-supplied `f`.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) for
Haskell, and the Haskell backend's source-stability limitation that idiom
permits.

Removing a `host fn` from a bridged module is a compatible env-side change at
the language level (see [`../versioning.md` § The compatibility
relation](../versioning.md#the-compatibility-relation)): an existing host
supplies a superset of what the new package requires, so the package still
loads. But on Haskell it is **not source-stable**, for a reason rooted in the
`<Handle>Host h m` boundary being a **nominal record**.

### Host-fn removal is not source-stable on Haskell

The host boundary is a Haskell record —
`data <Handle>Host h m = <Handle>Host { … }` — that the host constructs by naming its fields
(`<Handle>Host { host__… = … }`; copy the exact field from the declaration; see
[§ Host record contract](#host-record-contract)). Haskell records have **no
per-field default** and no partial-construction form that is both clean and
source-stable, so neither choice at a `host fn` removal preserves an unchanged
host:

- **Dropping the field** (the regenerated record declares one fewer field)
  makes an existing host's `<Handle>Host { …, host__… = … }` literal a GHC
  **compile error** — a record cannot be constructed naming a field its type no
  longer declares.
- **Keeping the field** (re-emitting the removed `host fn` as a record field,
  the way the Rust backend re-emits a `#[deprecated]` trait method) forces every
  *new* host to initialize it as well: a record has no default field value, so
  omitting it is a `-Wmissing-fields` warning and a `⊥` — converting a removal
  into a spurious supply obligation, the opposite of what the deprecation is
  for.

Rust's default trait method resolves exactly this (old impls override, new
impls inherit the default), but Haskell records have no analogue: **no escape
hatch keeps a removed field settable-but-optional**. This is a genuine
limitation of the Haskell record construct, not deferred work. The Haskell
backend therefore **re-emits nothing** for a removed `host fn`; the emitter
builds the `<Handle>Host h m` record from the *live* `host fn` set alone in
both `render_host_record` implementations: the universal fallback in
[`kio-rs/src/backends/haskell/emit.rs`](../../kio-rs/src/backends/haskell/emit.rs)
and the native path in
[`kio-rs/src/backends/haskell/native.rs`](../../kio-rs/src/backends/haskell/native.rs).
Both emitter comments cite this subsection in turn. A host migrating across a
`host fn` removal edits its `Host { … }` literal to drop the field.

Removing a `host type` uses the marker class's defaultable associated-type
idiom instead when its retained declaration epoch is compatible. The generated
class retains the removed declaration's exact associated-family member as a
compatibility-only member with a private, kind-correct default and a native
`{-# DEPRECATED type <family> "…" #-}`
pragma naming the removed Kio declaration and signature version. An existing
instance may retain its old equation because the family name still exists; a
new instance may omit the equation because the default supplies it.
Compatibility-only members are excluded from the live completeness constraint
and no live package signature refers to the default. The retained family adds
no host-record field, load/link requirement, adapter, package-handle
capability, or execution path. If the same exact host type is live, live
provenance wins: its family is required and has no deprecation pragma. This
preserves both sides of removal without confusing the type binding with a
host-function value.

### Incompatible retained declaration epochs are not source-stable on Haskell

One generated Haskell type identity cannot describe two incompatible exact
declaration epochs. Under the shared
[`README.md` rule](README.md#incompatible-retained-declaration-epochs), Haskell
omits every retained associated-family equation, callable root, and support
declaration that depends on the conflict. An old instance must delete or
rewrite an affected compatibility equation and any reference to an omitted
generated type. Current instances still define only live associated types;
removed host-function fields remain governed by the separate record caveat
above.

The shared degradation site is
`PreparedBoundaryCallableSitesCollector::preflight_retained_nominal_conflicts`
in [`boundary_facade.rs`](../../kio-rs/src/backends/boundary_facade.rs); its
comment cites the shared rule's explicit backend fan-out to this subsection.

Haskell remains **addition**-breaking, exactly as the cross-backend idiom notes:
a new `host type` adds a required associated-type choice, and a new `host fn`
adds a record field the host must initialize, so an unchanged host no longer
satisfies the new contract. The contravariant asymmetry holds, with the
host-function removal side additionally carrying the source-stability
limitation above.

## Boundary semantics

Each cross-cutting property is inherited from [`README.md`](README.md);
the Haskell instantiations:

- **Synchronous calling convention** ([`README.md`](README.md)) — a host
  call is a direct field application sequenced through `m`; an exported
  invocation is a direct function call returning `m a`. No async, no
  scheduler beyond the monad the host chooses.
- **Native HKT, no carrier erasure at the FFI** ([`README.md`](README.md))
  — the *skin* is fully typed and native. A kind-`*→*` carrier maps onto a
  Haskell type constructor (`f a`); a rank-N payload crosses at its
  `forall` type (`RankNTypes`), with each first-class `forall` stage yielding
  its next stage through `m`. No carrier-walk and no `Any`-erasure of
  the HKT carrier: the host's own kind system carries `F(A)` directly,
  where the erased families (`erased-static`, `dynamic`) instead ride the
  universal erased value. This is the **HKT-carrier** handling the
  native-HKT family does natively, per [`README.md`
  § Higher-kinded types](README.md#higher-kinded-types). Exact host types
  likewise remain their associated-family applications through generic
  and rank-N signatures.
- **Facade topology and execution provenance** ([`README.md`](README.md#facade-topology-and-execution-provenance))
  — one package-complete prepared catalog supplies every live host field,
  export, and public-newtype member, plus retained compatibility identities.
  Haskell realizes the catalog's exact sites, stages, slots, nested callback
  layouts, and structural keys directly; the paired execution layout alone
  rebuilds native source products and Unit values for the body. A removed host
  function has no retained record field and no package dispatch path. The
  separately retained compatibility-only host type is type-level and does not
  become a callable site.
- **Strict evaluation in a lazy host** — Kio is strict; Haskell is lazy.
  Effect order is carried by the monad: host effects are `m a` sequenced
  with `>>=`, so the order Kio specifies is the order the monad runs, by
  construction. Native `forall` carries no term argument. A direct known
  declaration may keep adjacent quantifiers compact; an escaped polymorphic
  value has one monadic adapter stage per quantifier. The adapter pure-lifts a
  work-free stage and sequences real computation at the stage where Kio
  performs it, before a following type or value application. Value strictness
  is imposed by forcing bound values: a
  `let` / `Seq` binding forces its value to weak-head normal form (`seq` /
  strict fields), so a bound Kio value is evaluated when the strict
  semantics says it is, not lazily on first use. This is the per-backend
  runtime concern, handled in the body and runtime support, **not** a spec
  carve-out.
- **Open-world property** ([`README.md`](README.md)) — adding a
  declaration to a module body never changes the meaning of existing
  emitted code. Every public callable and nested slot is selected by its exact
  module/declaration/site identity and stable boundary path; no ambient name
  occupancy or source-position search participates in the selection.
  Structural types use the namespace-derived product / sum families plus
  aliases derived from that exact boundary member and slot, so another
  declaration cannot rename or reinterpret an existing shape. A host-type
  family name and every nominal newtype head depend only on that declaration's
  exact module path and leaf. A newly bridged host declaration extends the
  host contract but never changes an existing declaration's identity.
- **Package isolation**, **exception propagation**, **well-foundedness
  inheritance**, **behavioral additivity** ([`README.md`](README.md)) —
  inherited unchanged. Each package exports its own marker class, so one
  local marker may implement two packages' contracts without merging
  same-named host types. Native `__absurd__` elimination is
  `Data.Void.absurd`. Because the universal body fallback erases bottom
  internally, both its `__absurd__` elimination and its internal-to-public
  bottom conversion use explicit unreachable failures after the producing
  effects have run. A host `exit(n)` propagates through the process exit code
  (the host's `exit` field maps to `System.Exit`).

## Body model

How the emitter renders the package body is an **emitter-internal** concern,
not part of the host contract: the typed FFI skin (§ Host record contract,
§ Package API, § FFI surface) is identical whichever way the body is rendered,
so a host never observes the choice. The rendering mechanics — the
native-vs-fallback strategy and its trigger cases — live in the emitter, not on
this page.

The boundary does not expose or derive authority from that internal choice.
Every concrete Kio host type remains the exact associated family selected by
`h`, and generic or rank-N boundary positions use native Haskell
quantification. An internal universal representation, when used, is not a
substitute for either public type identity.

Native body polymorphism preserves Kio's one-binder application tree through
two deliberately different representations. A statically known declaration
may use its direct Haskell scheme: adjacent declaration groups `[A][B](p: P)`
followed by an effectful body returning `R` use
`forall a. forall b. P -> m R`.

The resolved type alone does not locate the body action. Effectful declarations
can have the same call type `[A] P -> [B] Q -> R` while placing that action at
different stages. If `[B](q: Q)` is another declaration group, the direct
scheme is `forall a. P -> forall b. Q -> m R`: the body runs only after `Q`.
If the declaration groups end after `[A](p: P)` and its effectful body returns a
value of type `[B] Q -> R`, the direct scheme is
`forall a. P -> m (forall b. m (Q -> m R))`: that body runs after `P`, and the
returned first-class value supplies the later uniform binder stage.

An escaped or otherwise first-class polymorphic value uses a uniform internal
callable ABI with one monadic stage per `forall`. Thus `[A][B] P -> R` is
`forall a. m (forall b. m (P -> m R))`. An adapter from a direct declaration
pure-lifts the two work-free type stages and leaves the declaration call at the
value stage. Both `[A] P -> [B] Q -> R` declarations above adapt to
`forall a. m (P -> m (forall b. m (Q -> m R)))`: the first pure-lifts the
work-free stage after `P`, while the second preserves its real action there.
This uniform representation keeps same-typed values substitutable without
consulting their origin.
Boundary wrappers introduce or consume these internal stages while keeping
type parameters out of the term-level host ABI. In particular, the compact
native `forall`s in a public host-record field are its host-facing scheme, not
the representation of a first-class callable inside the package body.

A body fallback is admissible only when every public host-function slot,
exported function slot, and exported newtype member remains exactly
bridgeable through that representation. A private body construct may choose
a different internal route without changing the facade; an inexact public
slot makes emission fail rather than weakening its type.

## FFI surface

### Host-selected and atomic types

Every Kio `host type`, role-bearing or not, crosses as its exact associated
family applied to the host marker `h`. The host chooses the Haskell type. Roles
govern which literal or conditional syntax Kio admits; they are not a
representation registry. For example, two declarations carrying `role(str)`
may be bound to `[Char]` and `Data.Text.Text`, or to two distinct domain types,
provided the Haskell constraints induced by the package's actual string
literals are satisfied. Likewise, `role(i32)` does not impose a 32-bit Haskell
representation.

`()` (unit) is native Haskell `()`. Kio `!` is `Data.Void.Void`, including when
it appears inside a structural or nominal boundary type. A host eliminates an
incoming value with `Data.Void.absurd`; a host function returning `!` has an
`m Data.Void.Void` result and therefore cannot return normally. Numeric and
string constants are emitted as ordinary overloaded Haskell literals at their
selected associated types. Boolean constants become `True` / `False`, and a
Kio conditional becomes an ordinary Haskell conditional, so the selected type
in either position must be `Bool`. These requirements are reflected in the
exact per-function constraints described in § Host record contract.

### Structural and nominal types

The skin is driven through the shared FFI framework
([`README.md` § Structural FFI shape conventions](README.md), the
right-spine walk + 3-step key fallback). A compound type's Haskell shape:

- **Product** → the exported closed family whose preferred spelling is
  `<Handle>Product` (the structural collision rule in § Item naming may
  escape it). Writing that resolved family as `<ProductFamily>`, its equations
  are `<ProductFamily> '[] = ()`, `<ProductFamily> '[a] = a`, and
  `<ProductFamily> (a ': as) = (a, <ProductFamily> as)` for two or more slots.
  Thus `(A & B & C)` reduces to `(a, (b, c))`.
- **Sum** → the exported closed family whose preferred spelling is
  `<Handle>Sum` (with the same collision rule). Writing its resolved name as
  `<SumFamily>`, its equations are `<SumFamily> '[] = Data.Void.Void`,
  `<SumFamily> '[a] = a`, and
  `<SumFamily> (a ': as) = Either a (<SumFamily> as)` for two or more arms.
  Thus `(A | B | C)` reduces to `Either a (Either b c)`.
- **Boundary presentation** → every compound host-function slot has a
  stable exported `Env_H…` alias, and every compound exported-item slot has
  a stable exported `Exp_H…` alias. The alias applies the corresponding
  family to the right-spine slots and explicitly kinded lexical binders.
  A product alias exports one flat bidirectional record pattern with an
  `EnvP_H…` or `ExpP_H…` name, independently exported `envSel_H…` or
  `expSel_H…` selectors, and a one-pattern `COMPLETE` set. A sum alias exports
  one bidirectional unary `EnvS_H…` or `ExpS_H…` pattern per arm, encoding the
  arm's first-non-colliding 3-step key, and a `COMPLETE` set containing every
  arm. These
  patterns hide the nested pair / `Either` spelling without introducing a
  nominal carrier. A rank-N slot retains its `forall` type in the alias and
  pattern, including the `m` layer after each first-class type application.
- **Non-existential newtype** → with both members public, a nullary,
  non-recursive declaration is transparent, while every parametric or
  recursive declaration uses one declaration-local abstract nominal head.
  With neither member or exactly one member public, every declaration uses
  the abstract nominal head and hidden storage. Fully applying a parametric
  newtype at the boundary does not expand it to its payload; boundary
  signatures use the same head for unsaturated applications. Only public
  constructor and projector wrappers provide the corresponding conversion.
- **Existential newtype** → a native existential carrier, constructed from
  its exact payload and eliminated only through its rank-N CPS projector.
  The carrier is a nominal leaf inside the structural family application that
  contains it. Its universal arguments use their direct native
  representation, so applying the carrier to a structural type does not
  change the carrier's identity across the boundary.
- **Function value** → a native curried Haskell function `a -> … ->
  m ret` whose value-domain arguments are the shared canonical facade slots.
  A product domain is therefore curried positionally and direct Unit is
  nullary; the paired execution layout reconstructs the native body's original
  tuple/domain representation. Compound legs are converted across the
  boundary, and the function value itself crosses as a real Haskell function.
- **First-class polymorphic value** → `forall a. m next`, nested once per
  successive Kio `forall`. The host uses visible type application and binds
  the returned action before applying the next stage. This preserves the
  Kio type's application structure without adding a term-level type argument.
- A **type-variable** position in a generic host function becomes a
  native Haskell type variable under `forall`; a higher-kinded parameter
  keeps its corresponding Haskell kind. It is not replaced by a
  package-wide host box, and it adds no term-level argument. The boundary
  adapter converts the compact public scheme to the uniform internal callable
  when the function is used as a value, pure-lifting work-free type stages and
  preserving the actual host action at its declared value-call stage.

### Carve-outs

**None.** Haskell expresses every spec- and IR-admitted FFI shape its host
language can carry: exact host-selected types, structural products as pairs,
sums as `Either`, flat boundary patterns, function values as native functions,
and higher-kinded / rank-N carriers at their native Haskell types
— **including a functor/monad dictionary**, whether threaded internally or
crossing the host boundary as a `host fn` parameter / return. A parametric
dictionary newtype has the same declaration-stable nominal identity as every
other parametric newtype; its exported constructor and projector expose the
exact rank-N payload type. `kio@haskell` runs the full golden corpus.

An internal body representation (§ Body model) is not a carve-out: it cannot
change or erase the typed FFI skin the host observes.

## Item naming

### Declaration-local type names

Source-readable components use [shared word casing](README.md#source-derived-public-names)
(`do_work` → `doWork`, `Native_token` → `NativeToken`).
This conversion applies to primary names and to cosmetic readable suffixes,
never to the raw byte identities in an exact codec.

The host-type class is `<Handle>HostTypes h`, package-qualified through
`<Ns>`. Each exact Kio host-type declaration contributes
`HostType__<module-segment>__…__<leaf>` with word-cased components when every identity component is
boundary-safe — no `__`, leading `_`, or trailing `_` — and that name does not
overlap a fixed facade type claim. Otherwise it contributes
`HostType_H<hex(UTF-8(module\0leaf))>__<readable-module>__<readable-leaf>`.
The fallback hexadecimal component encodes the unsanitized bytes; its suffix
is cosmetic. The spelling depends only on the exact declaration identity and
the configured target namespace, never on the declaration's `role(...)`, the
Haskell type selected by the host, other package declarations, or encounter
order. A fallback that itself overlaps a fixed type claim uses the structural
escape below.

A non-existential newtype that requires a nominal carrier — because it is
parametric, recursive, opaque, or has exactly one public member — has the one
abstract head
`KioCarrier_H<hex(UTF-8(module\0leaf))>__<readable-module>_<readable-leaf>`.
This same `KioCarrier` family covers ordinary, function-payload, and
dictionary-shaped parametric newtypes; payload shape does not select another
nominal identity. An existential newtype instead uses
`KioExistential_H<hex(UTF-8(module\0leaf))>__<readable-module>_<readable-leaf>`.
Both names are declaration-local and are exported abstractly when a public
signature names them.

The structural families prefer `<Handle>Product` and `<Handle>Sum`, derived
from the namespace's final segment. If either preferred spelling overlaps an
existing generated type-name class, the structural collision rule below
selects its deterministic escaped spelling instead. Their equations are
specified in § FFI surface.

### Readable public names and exact fallback encoding

Host fields and exported wrappers ordinarily expose the source route as
word-cased, `__`-separated components:

- host field: `host__<module-segment>__…__<leaf>`;
- ordinary export: `export__<module-segment>__…__<leaf>`;
- newtype-member export:
  `export__<module-segment>__…__<newtype>__<member>`.

This primary form is used only when every source component is boundary-safe:
it contains no `__` and neither starts nor ends with `_`. Consequently every
`__` run denotes exactly one route boundary. Boundary-safe Kio module and value
names are lowercase-initial while boundary-safe newtype names are
uppercase-initial, so the newtype component also keeps ordinary exports and
member exports disjoint. A Kio newtype name carrying the optional leading
underscore is not boundary-safe and therefore uses the exact codec below; it
is never emitted raw into a Haskell constructor position. The role prefix
separates host fields from exports. These rules make the ordinary form
injective and reversible without consulting other declarations. A name with
any non-boundary-safe component uses the exact codec below instead. Internal
module functions, boundary aliases and patterns, selectors, and their
generated helpers always use the codec.

The codec's *semantic-role tags* distinguish kinds of Haskell declaration;
they are unrelated to Kio `role(...)` annotations.

All tags below are hexadecimal bytes. All identity strings in this codec are
raw source strings, not word-cased host spellings. `||` means byte concatenation.
`u32(n)` is the four-byte unsigned big-endian encoding of `n`, and
`text(s) = u32(len(UTF-8(s))) || UTF-8(s)`, where the length is in bytes.
A module path is encoded as
`module = u32(segment-count)` followed by `text(segment)` for every segment in
source order; a zero count denotes the no-module identity. Then:

- `module-item = module || text(leaf)`;
- `item = 01 || module-item` for an ordinary item;
- `item = 02 || module || text(newtype) || text(member)` for a newtype member.

The distinct item tags keep ordinary-item and newtype-member identities
disjoint independently of their cosmetic readable suffixes.

A boundary is `owner || root || u32(step-count)` followed by every encoded step
in order:

- environment owner: `01 || module-item`; exported-item owner:
  `02 || item`;
- argument root: `01 || u32(index)`; return root: `02`;
- type-application step: `01 || u32(index)`; product/sum-slot step:
  `02 || u32(index)`; callback-argument step: `03 || u32(index)`;
  callback-return step: `04`.

A structural key is `01 || text(newtype)` for a bare newtype,
`02 || module || text(newtype)` for a qualified newtype, or
`03 || u32(index)` for a positional key. A sum-pattern identity is
`boundary || key || u32(arm-index)`. A rank-helper pattern identity is
`01 || boundary` for a product or `02 || sum-pattern` for a sum.

The top-level semantic roles are:

| Semantic role | Haskell prefix | Namespace | Tagged payload after the format tag |
| --- | --- | --- | --- |
| Internal module function | `mod` | value | `01 01`, then `item(ordinary)` |
| Host-record field | `host` | value | `01 02`, then `item(ordinary)` |
| Export wrapper | `exp` | value | `01 03`, then `item` |
| Boundary alias | environment `Env`, export `Exp` | type | `10`, then `boundary` |
| Product pattern | environment `EnvP`, export `ExpP` | constructor | `20`, then `boundary` |
| Sum-arm pattern | environment `EnvS`, export `ExpS` | constructor | `21`, then `sum-pattern` |
| Product selector | environment `envSel`, export `expSel` | value | `22`, then `boundary`, `key`, and `u32(index)` |
| Rank-N view type | `ViewT` | type | `30`, then `pattern` |
| Rank-N view constructor | `ViewC` | constructor | `31`, then `pattern` |
| Rank-N no-match constructor | `NoMatch` | constructor | `32`, then `sum-pattern` |
| Rank-N view function | `view` | value | `33`, then `pattern` |

The fallback or internal name is
`<prefix>_H<hex(01 || tagged-payload)>__<readable>`, where the leading `01`
is the fixed format tag and the complete byte
identity formatted as lowercase hexadecimal. Uppercase prefixes are used for
Haskell type/constructor namespaces and lowercase prefixes for the value
namespace, so every result is a legal identifier in its declaration position.

The readable basis is deterministic but cosmetic. Each source component is
word-cased before fixed role delimiters are inserted. A module is its segments
joined by `_`; a module item is `<module>__<leaf>` (or just `<leaf>` for the
no-module identity); and a newtype member is
`<module>__<newtype>__<member>` (or `<newtype>__<member>` with no module). A
boundary appends `_arg<n>` or `_ret`, then
`_app<n>`, `_slot<n>`, `_cbarg<n>`, or `_cbret` for each step. A bare key is
its newtype, a qualified key is `<module>_<newtype>` (or just `<newtype>` with
no module), and a positional key is `pos<n>`. Product patterns append `_P`;
sum patterns append
`_S_<key>_<arm-index>`; selectors append `_Sel_<key>_<index>`. Rank-N helpers
reuse their product/sum pattern basis. Sanitization retains ASCII letters,
digits, `_`, and `'`, replaces every other character with `_`, truncates to
64 ASCII bytes, and uses `name` if the result is empty. Neither compiler nor
runner parses this suffix.

For example:

- host field `f` in module `a/b` is
  `host__a__b__f`;
- host field `f` in the distinct module `a_b` is
  `host__aB__f`;
- the ordinary export `a.foo_bar_` is
  `exp_H0101030100000001000000016100000008666f6f5f6261725f__a__fooBar_`,
  while member `bar` of the legal newtype `a.Foo` is
  `export__a__Foo__bar`;
- routes `a_/b.g` and `a/_b.g` both use distinct exact fallbacks rather than
  sharing the ambiguous primary candidate `host__a___b__g`; member routes
  `api.A_.b` and `api.A._b` follow the same rule rather than sharing
  `export__api__A___b`.

The two host-field names are distinct because one route has two module
components and the other has one. The export examples show the fallback for a
source affix and the ordinary spelling for a
newtype member; the final examples show the same fallback preventing an
underscore run from straddling a component boundary. None of these decisions
consults declaration occupancy.

Within a Haskell namespace, repeated generation deduplicates only the same
semantic identity with the same declaration. A repeated identity with a
different declaration, or distinct identities that render to one name, is a
compiler error rather than a suffix-allocation rule.

The namespace-derived facade names `<Handle>HostTypes`, `<Handle>Host`,
`<Handle>`, and `create<Handle>` retain the spellings specified in § Output
layout. Structural declarations use an immutable collision context derived
from the full target namespace. Their preferred spellings are retained unless
they overlap a fixed claim in the same Haskell namespace. The type claims are
`<Handle>`, `<Handle>Host`, `<Handle>HostTypes`, `Type`, `KioOpaque`, and the
resolved product and sum families; the constructor claims are `<Handle>` and
`<Handle>Host`; the value claims are `create<Handle>` and `pkgHost`. A
preferred type or constructor spelling also overlaps when it is exactly a
generated `KioCarrier` or `KioExistential` name; a preferred type spelling
additionally overlaps either an ordinary or fallback generated `HostType`
name.

An overlapping structural name or exact-host fallback is escaped as
`<escape-prefix>_H<hex(identity)>__<readable-preferred>`. The escape prefix is
`KioStructT` in the type namespace, `KioStructC` in the constructor namespace,
and `kioStructV` in the value namespace. Its byte identity is
`01 || text(full-target-namespace) || structural-identity`, where
`structural-identity` is `01` for the product family, `02` for the sum family,
`03 || tagged-payload` for a semantic-name entry from the table above, or
`04 || module-item` for an exact host type.
The readable suffix uses the same sanitization and bound as the ordinary
semantic-name suffix and carries no identity.

For example, target namespace
`KioCarrier_H6170690050726f64756374__api_` would make the preferred product
family `KioCarrier_H6170690050726f64756374__api_Product`, exactly the nominal
carrier for `api.Product`. The family therefore uses
`KioStructT_H01000000284b696f436172726965725f48363137303639303035303732366636343735363337345f5f6170695f01__KioCarrier_H6170690050726f64756374__api_Product`.

Neither public-primary eligibility, family selection, nor structural escaping
inspects package declarations or encounter order. Adding an unrelated
declaration therefore cannot rename an existing host binding, export, family,
alias, pattern, selector, or rank-N helper; the compiler and independent runner
reconstruct the same name from the target namespace and semantic identity
alone.

Field keys follow the 3-step key fallback ([`README.md`](README.md)); the
positional fallback is `_<i>`. The key is encoded into the relevant pattern or
selector identity rather than flattened into an identity-bearing suffix.

## Worked example

A package `greet` with a role-bearing string type, a role-free output
destination, two host functions, and an exported `main`:

```kio
// greet.pkg.kio
package greet;
build { cache "out/.kio-cache/"; target haskell { out "out/haskell/"; } }
bridge { app; app/**; }
```

```kio
// app.kio
module app;
host type Str role(str);
host type Output;
host fn stdout() -> Output;
host fn print(output: Output, s: Str) -> .;
```

```kio
// app/main.kio
module app/main;
import app(Str);
import app(print, stdout);
pub fn main() -> . { print(stdout(), "hello\n"(Str)) }
```

`kio build haskell` writes `out/haskell/Greet.hs` (the
package `greet` title-word-cases to the namespace `Greet`, so the handle is
`Greet`, the marker class `GreetHostTypes`, the host-function record
`GreetHost`, and the factory `createGreet`). A Haskell host selects every
host type before supplying the functions. Here `role(str)` is deliberately
bound to `[Char]` (`String`), demonstrating that the role does not force
`Data.Text.Text`:

```haskell
{-# LANGUAGE TypeFamilies #-}

import Greet (GreetHostTypes(..))
import qualified Greet

data Output = Stdout
data AppTypes

instance Greet.GreetHostTypes AppTypes where
  type HostType__app__Str AppTypes = String
  type HostType__app__Output AppTypes = Output

stubHost :: Greet.GreetHost AppTypes IO
stubHost = Greet.GreetHost
  { Greet.host__app__stdout = pure Stdout
  , Greet.host__app__print = \output s ->
      case output of
        Stdout -> putStr s
  }

main :: IO ()
main = do
  let pkg = Greet.createGreet stubHost
  Greet.export__app__main__main pkg
```

This prints `hello` and exits 0.

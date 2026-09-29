# Case study: dynamic loading (`dyn_load_prime`)

This page is a faithful read-through of the dynamic-loading proof-of-concept
package at
[`test-data/poc/dyn_load_prime/`](../../test-data/poc/dyn_load_prime/). That
package is executable, `kio test`-checked, and built and run against every
declared target in CI, so it is *ground truth*: every type, function, and
`equiv` law shown here exists in the package exactly as written, and the
program below is the one its `main` runs.

`dyn_load_prime` demonstrates that runtime loading of a pre-compiled Kio
package needs **no new language, package, or build feature**: the loader is
an ordinary Kio package a host links, and the image it loads is the plain
Kio' source text the `kio-prime` backend already emits — the whole emitted
tree, exactly as `kio build kio-prime` writes it. The capability rides
entirely on the existential type Kio' already has. The same loader is also
CI infrastructure: the dyn-load-prime differential
([`TESTING.md`](../../TESTING.md) § Test layers) loads hundreds of golden
images through it on every run and holds its interpreter to
compile-and-run's observable behavior.

For the task-oriented tour of the same loop — produce an image, then load,
contract-match, instantiate, and call it — read
[Dynamic loading](../guides/dynamic-loading.md).
This page is the single-program view: how the loader's pieces fit together
in one package whose `main` loads a guest from its emitted image and calls
every export shape.

## What the package contains

The package name is `dyn_load_prime`. Its `workdir/` is laid out like a
worked-example POC, but it is larger than most — a small interpreter and
loader split across focused modules:

- [`dyn_load_prime.pkg.kio`](../../test-data/poc/dyn_load_prime/workdir/dyn_load_prime.pkg.kio)
  — the package file: a `build { ... }` block targeting `js`, `ts`,
  `python`, `java`, `rust`, `go`, `swift`, and `haskell`, and a
  `bridge { ... }` block exposing the loader's modules plus the `testapi`
  host surface.
- [`testapi/main.kio`](../../test-data/poc/dyn_load_prime/workdir/testapi/main.kio)
  — the worked example: it builds the image of a guest package with
  thirteen fn exports and one member-exporting `pub newtype` (guest
  module, host module, and manifest), loads it with `load_package`,
  calls every export shape through the loaded surface — the newtype's
  constructor and projector included — enumerates the export surface, and
  runs three contract-matches and one contract-gated call. This is the
  module whose `main` the runner invokes.
- The loader library is split across `token` / `chars` / `lex`
  (lexing), `prime` (the Kio' term representation), `value` (the
  interpreter's runtime values), `result` (the evaluator outcome types),
  `parse` (term parsing and scope resolution), `eval` (the CEK evaluator), `marshal`
  (higher-order call-result adaptation), `hostrec` (the standard `call`,
  `make_lit`, and `test_bool` capabilities), `loader` (resolved images,
  instantiation, and loaded surfaces), `loader/scan` (the public
  `load_package` source-text entry point), `defaults` (the
  standard-capability entry points), and `imageio` (reading an image from
  the host's input channel).
- `testapi.kio` and `testapi/{arith,fmt,io,iter,scalar,text}.kio` —
  host-boundary scaffolding (`host type` / `host fn` declarations) standing
  in for the capabilities a real host supplies. A user replaces this
  scaffolding with their own host.
- Two package [dependencies](../guides/pkg.md#dependency-files),
  brought in the standard way any package reuses another: `elab.dep.kio`
  vendors the [`elab` POC](../guides/using-libraries.md) (the
  `match!` / spine-elaborator machinery the loader's own sources use,
  re-rooted under `elab/`), and `list.dep.kio` vendors the `list` POC
  (the generic list library the loader's internals are built on, under
  `list/`). Each dependency's host requirements are rehosted onto the
  package's own capability modules through the small `list_host.kio`
  adapter, and each materialized tree is committed and held
  live-equivalent to regeneration by the `dep-canonical` check.

Running the package checks its public signature, then chains `kio check`,
`kio test`, the per-backend build, and the runner; `main` prints the report
shown at the end of this page.

## The image: the whole `kio-prime` build output

The package a host loads is the output of compiling a package to the
`kio-prime` target. `load_package` consumes that tree **as-is**: the
concatenated text of every emitted `.kio` file — regular modules, host
modules, and the `.pkg.kio` manifest alike, in any order. Nothing is
curated away:

- Each `module` / `package` header delimits one unit, and the loader
  topologically orders the modules from their `import` headers.
- A **host module** — all `host fn` / `host type` declarations, no bodies —
  contributes no guest code; its declarations tell the loader which
  imported names are host capabilities the instantiating host serves.
- The **manifest**'s `bridge { … }` block defines the export surface: a
  plain-public function in a bridge-matched module is a host-invocable entry,
  a plain-public newtype contributes its nominal identity, and each
  plain-public constructor or projector contributes a callable member export.
  An unmatched module's items are internal and unreachable from the host,
  under both their bare and their module-qualified names
  ([`specs/package.md` § The bridge block](../../specs/package.md#the-bridge-block)).
  A concatenation with no manifest still loads, treating every module as
  bridged when deriving that public callable and type surface.

The worked example exercises the whole contract: its guest image is a
three-file tree — guest module, host-declaration module, and manifest —
assembled exactly as the emitter writes it.

## Loading: image text in, a resolved image out

`loader/scan.load_package` is the public source-text entry point. It turns raw
image text into module records, then calls the resolver scoped to
`loader.kio`; hosts do not receive either intermediate surface.
The loading pipeline lexes the text, scans each module's headers and
declarations, topologically sorts the modules, builds each module's visibility
(imports, aliases, and its table of visible newtypes and fns), resolves every
declaration body to an erased Kio' core term, and records the host requirements
and the bridge surface. The result is a
`Load_outcome` — opened with `un_load_outcome` into an `Lr_image` (a
resolved `Image`) or an `Lr_diag` (a `Diag`).

Loading does not run the guest. And a malformed image is never a silent
`()`: every load failure is a `Diag` whose message names the module, the
declaration, and — where the text itself is at fault — the line and column
of the offending construct. The dedicated
`exec_dyn_load_diagnostics` golden pins these messages, from an
unterminated string literal's position to a dependency cycle's member
list.

`Image`, `Export_info`, and `Export_infos` expose their types and read-only
accessors, but keep their raw constructors and projectors inside the loader
module. A host can inspect an image through the public `image_*` readers and
walk its validated export table with `un_export_infos`; it cannot manufacture
one that skipped the loading checks.

The loader reads the **emitted grammar, in full**: wrapped leading-comma
headers and import lists, adjacent chunked string literals, the full
escape set, float exponents, keyword-spelled declaration names (Kio has no
reserved words), qualified and aliased imports, `pub(path)` visibility,
multi-group curried signatures, existential binder runs, and type-only
partial applications. Each of those shapes is pinned by a dedicated
`exec_dyn_load_*` golden that builds a guest at test time with the live
compiler and loads the fresh emit — so emitter drift breaks a golden, not
a user.

Semicolon-delimited bodies keep separators between peer declarations or
expressions; a leading or trailing separator is accepted but does not change
the body. A completed outer braced declaration may likewise carry one
redundant following semicolon. Canonical emitted Kio' omits those edge tokens,
as specified in the [grammar](../../specs/grammar.md) and [style
contract](../../specs/style.md#semicolon-clause-blocks).

## The trust model

Loading **trusts the image's function bodies**, exactly as build-time
linking trusts a compiled artifact — deliberately, so loading is a single
pass over the text. The loader re-typechecks nothing at load time and does
not read the package's `.sig.kio` versioning changelog. Use it only with
precompiled output whose producer and build process you trust: `kio build
kio-prime` typechecks the package before emitting the image, but `load_package`
cannot establish body correctness for source modified or obtained afterward.
Written bindings are structural checks: an ordinary type, value, or module-alias
name may be introduced once, even when repeated imports select the same
provider. An ordinary transparent alias retains its type and member identity
but does not merge its introduction with a selective import. The fixed
`__intrinsics__` block remains idempotent.
The runtime gates are the exact
host-requirement preflight and the typed export contract-match below; both run
before any call crosses.

The image also carries its complete host requirements. Each host type and host
function is identified by its declaring module and declaration-local name;
host-function requirements additionally pair a canonical resolved signature
with the ordered right-spine application-group sizes independently derived
from the resolved type shape (a whole unit group contributes zero values).
The loader never recovers those sizes by parsing canonical signature text.
Every function has at least one application group and every size is
nonnegative; a required adapter binding or `hostfn_in` binding with an invalid
shape is rejected at that boundary.
Transparent aliases are expanded, binders alpha-normalized, and nominal and
host identities fully qualified. Host-type requirements carry their parameter
kinds or role metadata. Header validation rejects higher-kinded host-type
parameters and role-bearing host types with parameters, matching the source and
Kio' checkers even though function bodies remain trusted. The host supplies an
adapter that advertises the exact requirements it offers.
The image's public type surface is closed over those exact identities too.
Every callable export signature and plain-public transparent alias may reach
only plain-public aliases and newtypes from bridged modules and host types from
bridged modules. Alias expansion preserves the selected alias identity and the
dependencies of every supplied type argument for this validation, even when an
argument does not occur in the expanded body. A plain-public newtype contributes
its opaque nominal identity without exposing its payload. Its payload joins the
closure only when a plain-public constructor or projector contributes the
corresponding callable export; with both members private, public functions may
still carry the nominal value without revealing its representation.
Instantiation checks the image's whole required inventory against that adapter
before evaluating any guest binding. Extra advertised inventory entries are
harmless, but a missing declaration or a same-identity descriptor mismatch is a
diagnostic. Because the loader does not typecheck bodies, the trusted producer
is responsible for ensuring that role metadata admits a literal or
`role(bool)` conditional. Separately from the adapter inventory, `instantiate`
receives the runtime `call`, `make_lit`, and `test_bool`
capabilities. The loader passes `call` the exact host-function descriptor,
passes `make_lit` the literal text and exact host-type descriptor from which the
host chooses a representation, and passes `test_bool` the exact host-type
descriptor and opaque scalar from which the host chooses a branch. None of
these capabilities recovers an identity or representation from a role.

## Instantiation: evaluate once, then an existential surface

`instantiate` (and its standard-host specialization
`instantiate_default`) first validates the image's exact host requirements,
then evaluates every top-level binding **exactly once**,
in dependency order — host-fn seeds outermost, then each guest binding in
the resolved global order, each evaluated in the environment of the ones
before it. The result is an environment that export lookup reads by
position; no per-lookup re-evaluation happens. A top-level binding whose
evaluation goes stuck fails instantiation with a `Diag` naming the
declaration and the reason — which is why both entry points answer
`Loaded | Diag`.

`Loaded` packs the existential: the representation `P` of a loaded value
is hidden, and the host receives `Surface(P)`, a record of everything it
may do:

```text
pub type Surface[P] =
  & Lookup(P)
  & Apply(P)
  & Apply_fuel(P)
  & Unit_in(P)
  & I32_in(P)
  & I32_out(P)
  & F64_in(P)
  & F64_out(P)
  & Str_in(P)
  & Str_out(P)
  & Bool_in(P)
  & Bool_out(P)
  & Pair_in(P)
  & Pair_out(P)
  & Left_in(P)
  & Right_in(P)
  & Sum_is_left(P)
  & Sum_payload(P)
  & Hostfn_in(P)
  & Exports
  ;
```

Each label is a parameterized operation type — `lookup` resolves an
export by (optionally module-qualified) name, `apply` / `apply_fuel`
apply a loaded closure (with the default or an explicit evaluation
budget), the `*_in` / `*_out` pairs marshal unit, i32, f64, string, and
bool values in and out, `pair_in` / `pair_out` build and split loaded
products, `left_in` / `right_in` / `sum_is_left` / `sum_payload` inject
into and project out of loaded sums, `hostfn_in` looks up an exact
host-function descriptor and answers a guest-callable value or a diagnostic
(the callback story below), and `exports` enumerates the surface.
Every fallible operation answers `… | Diag` with a reason — an absent or
ambiguous export, a stuck evaluation and why, a wrong-shape value — so a
host reports *what* went wrong, not merely that something did.

The worked example calls each export shape through this surface: a
`String -> String` greeting, i32 arithmetic through single- and
multi-parameter exports, an f64 flow seeded by a nullary export
(`scale(default_gain())`), a product-returning `divmod` split with
`pair_out`, and a sum-returning `classify` consumed with `sum_is_left` /
`sum_payload`. It also shows the informative failure: applying the
string-domain `greet` to an i32 prints the stuck reason naming the host fn
and the shapes involved.

## Type exports: a newtype's members on the surface

A bridge-matched module's `pub` surface is not only its fns. A
`pub newtype` whose members are `pub` puts its constructor and projector
on the loaded surface as callable exports, matching the compiled facades'
treatment of the same declarations. The guest declares:

```text
pub newtype Tag[A] : A { pub constructor mk_tag; pub projector un_tag }
```

and the surface gains two entries named module, type, and member —
`guest.Tag.mk_tag` and `guest.Tag.un_tag` — driven through the same
`lookup` / `apply` ceremony as any fn export. `lookup` accepts both the
qualified spelling and the bare `Tag.mk_tag`, under the same ambiguity
rule as fn names: a bare spelling matching entries in more than one
module answers with a `Diag` that lists every qualified candidate.
Visibility gates members like everything else on the surface — a member
that is not plain `pub` (private, or `pub(path)`-scoped) is not an
export, and neither member of a newtype in a module the bridge does not
match is. At runtime the newtype erases: both loaded members are the
identity, so `un_tag(mk_tag(11))` rides the i32 through unchanged — the
`un_tag(mk_tag(11)) = 11` line of the report.

## Host callbacks: capabilities as first-class guest values

A higher-order export takes a function argument, and the host supplies one with
two matching pieces: the `Host_adapter` inventory advertises its exact
descriptor and application-group sizes, while the separately supplied `call`
capability implements it. The interpreter's values are data — Kio enforces
strict positivity, so a runtime value cannot embed a host closure — and
`hostfn_in` mints a guest-callable handle for the advertised descriptor. It
accepts the callback's exact declaring module, local name, and canonical
resolved signature. A curried callback is not dispatched before its final
advertised group is complete. The worked example extends the standard adapter
with one exact binding and wraps the standard dispatch with one extra arm:

```text
fn host_double_descriptor() -> Host_fn {
  mk_host_fn(
    , mk_host_id("testapi/main"(String), "host_double"(String))
    , "(h{hostapi.I32}) -> h{hostapi.I32}"(String)
    )
}

fn serve_with_callbacks(call: Host_fn & Value) -> Call_result {
  match! call {
    .(descriptor: Host_fn, dom: Value) {
      if! host_fn_eq(descriptor, host_double_descriptor()) {
        host_double(dom)
      } else {
        serve_default(descriptor, dom)
      }
    }
  }
}
```

passes the extended adapter and wrapped `call` capability to the explicit
`instantiate` entry point, then
passes the minted handle into the guest's `apply_cb` export — the
`apply_cb(host_double, 21) = 42` line of the report. `hostfn_in` rejects a
descriptor the adapter did not advertise before a handle is minted; a
callback signals its own failure by answering `Call_result`'s stuck arm,
which surfaces during application. The callback set is fixed per
instantiation: the adapter inventory is checked once, so a host enumerates the
closures it intends to pass before instantiating, and the matching `call`
capability implements them at runtime.

## Under the surface: the interpreter

`value.kio` defines the runtime `Value` — a `newtype`-wrapped sum
(`Value_shape`) over a closure (`vclosure`), a product (`vproduct`), a
tagged sum (`vsum` / `vleft` / `vright`), unit (`vunit`), an opaque host
scalar (`vhost`, projected back with `value_to_scalar`), and a
partially-applied host fn (`vhostfn`). `result.kio` carries the evaluator
outcomes: `Eval_result` is `Ev_done | Ev_stuck`, with the stuck arm
carrying its reason as text, and the wider `Call_result` a host
capability answers adds the machine re-entry arms — `Ev_fold` drives an
interpreted loop step, `Ev_apply` applies a guest closure once (the
result is the host call's result), and `Ev_apply_then` applies and then
re-dispatches an exact host descriptor with the result, the
defunctionalized continuation shape strict positivity forces. `eval.kio` is the CEK evaluator: `prime_eval` reduces
a closed Kio' term to a value, driven by an explicit budget so a malformed
image exhausts fuel instead of hanging the host.

A guest's host effects flow through three capabilities implemented in
`hostrec.kio`: `serve_default` is the `call` capability and dispatches a host-fn
call by exact identity, `make_lit_default` builds a host scalar from literal
text plus an explicit exact-type mapping, and `test_bool_default` decides a
conditional. Each is backed by the package's own `testapi/*` host fns, which a
real host replaces. `defaults.kio`'s `eval_default` / `apply_default` run a term
or an application with those standard capabilities.

## Typed contracts: the runtime bridge

Before presenting the surface, a host **contract-matches** the image
against the interface it expects. A `Contract` is a list of expectations
(built with `mk_expect` over `contract_cons` / `contract_nil`), each
pairing an export name with its **full signature** in the canonical
rendering — copied from `image_exports(im)`, which is the image's canonical
export table. Host identities use `h{module.Type}`, nominal identities use
`n{module.Type}`, and binders use positional names such as `#0`. The host does
not ask the loader to resolve source-local type names:

```text
mk_expect("add3"(String), "(h{hostapi.I32} & (h{hostapi.I32} & h{hostapi.I32})) -> h{hostapi.I32}"(String))
```

`contract_check` compares every expectation against the declared surface:
present, on the bridge, and signature-identical, or the result is a `Diag`
that names the entry and shows **both** signatures. Member exports are
stated like any other entry — the worked example's matching contract
includes the newtype's constructor bare and its projector qualified:

```text
mk_expect("Tag.mk_tag"(String), "[#0] (#0) -> n{guest.Tag}(#0)"(String))
mk_expect("guest.Tag.un_tag"(String), "[#0] (n{guest.Tag}(#0)) -> #0"(String))
```

`instantiate_checked` / `instantiate_checked_default` fold the check into
instantiation and hand back a `Loaded` only on a full match. The worked
example states a matching
contract, a missing-export contract, and a wrong-signature contract, and
prints each outcome — the three lines near the end of the report below.

## The typed wrapper: ceremony once, typed calls everywhere

The surface is representation-hiding, so a raw call is a lookup, a
marshal-in, an apply, and a marshal-out, each with a `Diag` case. The
worked example writes that ceremony **once per export** behind a typed
signature — the pattern a real host follows. Its `greet` wrapper:

```text
fn run_greet[P](surf: Surface(P), who: String) -> String {
  match! surf.?{lookup}("greet"(String)) {
    (
      , .(g: P) {
          match! surf.?{apply}((g, surf.?{str_in}(who))) {
            (
              , .(r: P) {
                  match! surf.?{str_out}(r) {
                    (.(s: String) { s }, .(d: Diag) { diag_message(d) })
                  }
                }
              , .(d: Diag) { diag_message(d) }
              )
          }
        }
      , .(d: Diag) { diag_message(d) }
      )
  }
}
```

The wrapper is generic in `P` — it can never learn the representation —
and everything it touches is the surface record, accessed with row-record
field reads (`surf.?{lookup}`). Sibling wrappers in the worked example
swap the marshalling pair per export shape (`i32_in` / `i32_out`,
`pair_in` for the product-domain `add3`, `pair_out` for `divmod`,
`sum_is_left` / `sum_payload` for `classify`) around the same skeleton.

## The algebraic laws the package discharges

The interpreter's data types are `newtype`-wrapped sums, and the package
pins their **constructor / destructor round-trips** with `equiv` blocks
that `kio test` discharges on every CI run. `value.kio` carries the
runtime-value laws — every `un_value . v<arm>` recovers the arm it built:

```text
equiv un_closure(e: Env, t: Term) {
  un_value(vclosure(e, t));
  widen_sum!({v_closure = (e, t)}, Value_shape)
}

equiv un_product(a: Value, b: Value) {
  un_value(vproduct(a, b));
  widen_sum!({v_product = (a, b)}, Value_shape)
}
```

and the `Side` selector laws pin the two-way branch:

```text
equiv side_left[A](l: A, r: A) {
  side_select(l, r, s_left());
  l
}
```

`prime.kio` discharges the same shape for the Kio' term representation,
`token.kio` for the token shapes, `parse.kio` for the scope and
visibility-table shapes, and `result.kio` for the evaluator outcomes. Each
law is a destructor inverting its constructor — the algebraic backbone the
loader's correctness rests on, discharged alongside the differential
runner that holds the whole interpreter to compile-and-run's behavior.

## What `main` prints

Running the package assembles and loads the guest image once. It reuses one
default instance for all fourteen ordinary surface flows; the callback-capable
and contract-gated flows each use their distinct instantiation path. It then
enumerates the exports and contract-matches the image. The loaded host module
also places `add_i32` between two private ordinary functions; loading succeeds
only when the later function sees that newly declared host capability:

```text
Hello, World
quad(5) = 20
use_poly(42) = 42
use_add3(10) = 30
add3(3, 4, 5) = 12
pick(.t, 7, 9) = 7
pick(.f, 7, 9) = 9
scale(default_gain()) = 0.375
divmod(17, 5) = 3 r 2
classify(4) = small
classify(42) = 42
greet(3) = dyn_load_prime: the call got stuck — host fn `string_concat` expected two string arguments
apply_cb(host_double, 21) = 42
un_tag(mk_tag(11)) = 11
exports:
  guest.greet : (h{hostapi.String}) -> h{hostapi.String}
  guest.double : (h{hostapi.I32}) -> h{hostapi.I32}
  guest.quad : (h{hostapi.I32}) -> h{hostapi.I32}
  guest.id_poly : [#0] (#0) -> #0
  guest.use_poly : (h{hostapi.I32}) -> h{hostapi.I32}
  guest.add3 : (h{hostapi.I32} & (h{hostapi.I32} & h{hostapi.I32})) -> h{hostapi.I32}
  guest.use_add3 : (h{hostapi.I32}) -> h{hostapi.I32}
  guest.pick : (h{hostapi.Bool} & (h{hostapi.I32} & h{hostapi.I32})) -> h{hostapi.I32}
  guest.default_gain : () -> h{hostapi.F64}
  guest.scale : (h{hostapi.F64}) -> h{hostapi.F64}
  guest.divmod : (h{hostapi.I32} & h{hostapi.I32}) -> (h{hostapi.I32} & h{hostapi.I32})
  guest.classify : (h{hostapi.I32}) -> (h{hostapi.String} | h{hostapi.I32})
  guest.apply_cb : ((h{hostapi.I32} -> h{hostapi.I32}) & h{hostapi.I32}) -> h{hostapi.I32}
  guest.Tag.mk_tag : [#0] (#0) -> n{guest.Tag}(#0)
  guest.Tag.un_tag : [#0] (n{guest.Tag}(#0)) -> #0
contract (match) = ok
contract (missing export) = dyn_load_prime: the image's surface declares no exported `triple`
contract (signature) = dyn_load_prime: contract mismatch — export `guest.add3` declares `(h{hostapi.I32} & (h{hostapi.I32} & h{hostapi.I32})) -> h{hostapi.I32}` but the host expects `(h{hostapi.I32} & h{hostapi.I32}) -> h{hostapi.I32}`
checked quad(5) = 20
```

The `greet(3)` line and the two contract-mismatch lines are the loader's
own diagnostics — the informative failures a host sees when a call or a
stated contract does not match the image, standing in for the build-time
type error a linked package would have caught.

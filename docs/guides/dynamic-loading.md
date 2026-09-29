# Dynamic loading

Kio packages normally come online at **host-build time**: the host links a
package, the compiler emits the package into the host language,
and the host calls the package's exported items. Dynamic loading is the
other way in. A host can take a **pre-compiled** package, bring it online
**at runtime**, check that it offers the interface the host expects, and
call its exported items — all without rebuilding the host.

This guide walks the whole loop, both halves:

1. **Produce** the package as a loadable image — compile it to the
   `kio-prime` target.
2. **Consume** the image — load it, contract-match it, instantiate it, and
   call exports through the loaded surface.

The loader is an ordinary Kio package, `dyn_load_prime`. There is no new
language feature and no build-system magic: the image is plain Kio' source
text, and the loader is Kio code a host links once and then reuses for
every image it loads. The complete, runnable end-to-end is the
[`dyn_load_prime` case study](../poc/dyn_load_prime.md); this guide is the
task-oriented tour.

## The image is the `kio-prime` build output

The package a host loads is the output of compiling a package to the
**`kio-prime`** target. That emitted tree **is** the image. There is no
separate "image format" and no `dynamic` build setting: you build to
`kio-prime` like any other target, and what comes out is what a host
loads.

To produce an image, add a `kio-prime` target to the package you want to
ship:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target kio-prime {
    out "out/kio-prime/"
  }
}

bridge {
  main
}
```

Then build that target:

```sh
kio build kio-prime
```

`out/kio-prime/` now holds the image: one `.kio` file per module, written
in Kio' — the small, fully-desugared core — plus the emitted `.pkg.kio`
package manifest. The files are human-readable source you can open and
inspect, not an opaque blob. The same source text is what the loader reads.

## What the loader consumes

`load_package` takes the emitted tree **as-is**: the concatenated
text of every `.kio` file under the image directory — regular modules,
host modules, and the `.pkg.kio` manifest alike. You do not curate a
module subset, strip host declarations, or feed an entry point; the loader
sorts out the roles itself:

- **Module boundaries and order.** Each `module` / `package` header
  delimits one unit, and the loader topologically orders the modules from
  their `import` headers — the files may arrive in any order.
- **Host modules.** A module whose declarations are all `host fn` /
  `host type` supplies no bodies; its declarations tell the loader which
  imported names are host capabilities the instantiating host must serve,
  rather than guest functions to resolve.
- **The manifest.** The `.pkg.kio` manifest's `bridge { … }` block defines
  the package's contract surface (see the next section). A concatenation
  without a manifest still loads — every module is then treated as bridged
  when deriving its public callable and type surface.

## The export surface is the bridge

The manifest's `bridge { … }` block selects the modules that form the
host boundary ([`specs/package.md` § The bridge
block](../../specs/package.md#the-bridge-block)). The loader enforces the
same rule the compiled facade lives by: a bridge-matched module's
plain-public functions are host-invocable exports, its plain-public newtypes
contribute nominal type identities, and each plain-public constructor or
projector contributes a callable member export. An unmatched module's items
are internal to the package and **not reachable from the host**, under either
their bare or their module-qualified names. A newtype member that is not plain
`pub` (private, or `pub(path)`-scoped) stays off the surface even when its
outer newtype and module are public.

Exports keep their module namespace. A fn export is addressable as
`path/to/module.name` and by its bare leaf name; a member export as
`path/to/module.Type.member` and the bare `Type.member`. When two bridged
modules export the same spelling, asking for the bare form is answered
with a diagnostic naming the qualified candidates.

## Link the loader

The consumer side is a host that links `dyn_load_prime`. You bring the
loader in by **depending on its package** — declare a `<local>.dep.kio`
file for it, the same way any package reuses another; the dependency form
(local-path or git source, and the lock file) is documented once in
[`pkg.md` § Dependency files](pkg.md#dependency-files).
A dependency's materialized module tree is **committed** alongside its
`<local>.dep.kio`, so once you have vendored the loader your checkout is
self-contained — the loader's modules build with no fetch step (see
[§ The materialization model](pkg.md#the-materialization-model)).
The [case study](../poc/dyn_load_prime.md) and the package under
[`test-data/poc/dyn_load_prime/`](../../test-data/poc/dyn_load_prime/) show
the module set the loader exposes. Once linked, the loader's API is
ordinary `pub fn`s your host calls.

## Load the image text

Read the image's emitted Kio' source text — the bytes from
`out/kio-prime/` — and hand it to the loader. You get back a **load
outcome**: a loaded image, or a diagnostic when the text is malformed.

```kio {ignore}
// `image_text` is the emitted tree the host read from out/kio-prime/.
match! un_load_outcome(load_package(image_text)) {
  .(im: Lr_image) { /* a loaded image — check and instantiate it below */ };
  .(d: Lr_diag) { /* malformed image: report the diagnostic */ }
}
```

Loading does not run the package. It lexes the image text, scans each
module's top-level declarations, resolves every body into the core terms
the evaluator consumes, and records the host requirements and the bridge
surface — producing an **image** value you go on to instantiate. A
malformed image is answered with a `Diag` naming the module, the
declaration, and the line and column of the offending construct.

Only the loader constructs that image and its validated export table. Host
code can inspect the result through the public `image_*` readers and walk
`image_exports(im)` through `un_export_infos`, but it cannot mint an image or
an export-table entry that bypasses loading and validation.

## The trust model

Loading **trusts the image's function bodies**, exactly as build-time
linking trusts a compiled artifact — deliberately, so that loading stays a
single pass over the text. The loader does not re-typecheck bodies at load
time, and it does not consult the package's `.sig.kio` signature ledger.
Use the loader only with precompiled output whose producer and build process
you trust. `kio build kio-prime` typechecks a package before emitting this
image, but `load_package` cannot establish body correctness for source that was
subsequently modified or obtained from an untrusted producer. Three runtime gates
still protect the host boundary, but none typechecks function bodies:

- **Exact host-requirement preflight.** The image records every required host
  type and function by declaring module and local name, including parameter
  kinds or role metadata for types and a canonical resolved signature for
  functions. Canonicalization expands transparent aliases, alpha-normalizes
  binders, and fully qualifies nominal and host identities. Before evaluation,
  the loader compares that complete inventory with the host adapter's offered
  inventory. Each function entry also records the number of runtime values in
  each ordered right-spine application group, independently derived from the
  resolved type shape rather than parsed from its canonical signature text (a
  whole unit group contributes zero values). Every function has at least one
  such group and every count is nonnegative; an invalid required-adapter or
  explicit-callback binding is a diagnostic.
  Missing or mismatched entries are diagnostics; unrelated extra inventory
  entries are accepted. Because the loader does not typecheck bodies, the
  trusted producer is responsible for ensuring that role metadata admits each
  literal and `role(bool)` conditional. The runtime capabilities are supplied
  separately from the adapter inventory:
  `call` receives the exact
  host-function descriptor, `make_lit` receives the literal text and exact
  host-type descriptor and chooses its representation, and `test_bool` receives
  the exact host-type descriptor and opaque scalar and chooses the branch. These
  capabilities never recover an identity or representation from a role.
- **Closed public type surface.** Every callable export signature and public
  transparent alias is closed over plain-public types from bridged modules and
  host types from bridged modules. Transparent aliases expose their expanded
  shape, while their own identity and every supplied type argument remain
  dependency edges, including arguments the alias body does not use. A public
  newtype is different: its outer declaration exposes an opaque nominal
  identity, not its payload. The loader follows the payload only through a
  plain-public constructor or projector, whose callable signature exposes that
  payload. With neither member public, the nominal type may still cross public
  functions without revealing its representation.
- **Contract-match at the boundary.** What the loader checks at runtime is
  the **surface**: that the exports the host intends to call exist, are on
  the bridge, and have the signatures the host expects. That check is the
  next section.

This split keeps "type errors caught at compile time" honest: the image's
internal type errors were caught at its own `kio build`; the host's calls
into the loaded package are typed against the host's own wrapper (below)
and checked at the host's compile time; and contract-match verifies the
two sides name the same signatures before any call crosses.

## Contract-match what the host expects

Before calling into a loaded image, a host states a **contract** — the
exported entries it intends to call, each named with its full signature —
and has the loader check the image honestly offers them. An expectation
pairs an export name with the signature's canonical rendering:

```kio {ignore}
let c = contract_cons(
  , mk_expect(
      , "add3"(String)
      , "(h{hostapi.I32} & (h{hostapi.I32} & h{hostapi.I32})) -> h{hostapi.I32}"(String)
      )
  , contract_nil()
  );
```

`image_exports(im)` is the authority for these strings. It lists each callable
export with the exact canonical signature the host copies into its contract:
host types are `h{module.Type}`, nominal types are `n{module.Type}`, products
and sums retain their structure, and binders are positional (`#0`, `#1`, ...).
The host does not submit a source-spelled signature for the loader to resolve.

`contract_check(im, c)` compares each expectation directly against that image
surface. Every expected entry must be present, on the bridge, and
signature-identical, or the check answers a `Diag` naming the offending entry
and showing both exact strings. `instantiate_checked` /
`instantiate_checked_default` fold the check into instantiation and only
hand back a usable handle on a full match.

The instantiated surface's `exports` field carries the same listing for code
that has already opened a loaded handle.

## Instantiate and call an export

Instantiating evaluates every top-level binding exactly once, in
dependency order, and presents the image's exports at an **existential**
interface: `Loaded` packs `exists P. Surface(P)`. The representation `P`
of a loaded value is hidden; `Surface(P)` is a record of operations over
`P`:

- `lookup` — resolve an export by (optionally module-qualified) name.
- `apply` / `apply_fuel` — apply a loaded closure to a loaded value,
  with the default or an explicit evaluation budget.
- `unit_in`, `i32_in` / `i32_out`, `f64_in` / `f64_out`, `str_in` /
  `str_out`, `bool_in` / `bool_out` — marshal host scalars (and the unit
  value) into and out of `P`.
- `pair_in` / `pair_out` — build a loaded product, and split one.
- `left_in` / `right_in`, `sum_is_left` / `sum_payload` — inject into and
  project out of a loaded sum.
- `hostfn_in` — look up an exact host-function descriptor in the adapter and
  mint a guest-callable value, or answer a diagnostic when the adapter does
  not offer it. A callback implemented by the separately supplied `call`
  capability crosses into higher-order exports like any other value.
- `exports` — the enumerated export surface, names and signatures.

Every operation that can fail — a lookup, an application, a scalar
projection — answers `… | Diag` with a reason (an absent or ambiguous
export, a stuck evaluation and why, a value of the wrong shape), so a host
can report *what* went wrong rather than merely that something did.

`instantiate` takes an explicit host adapter for the exact-inventory preflight.
It separately takes the `call`, `make_lit`, and `test_bool` capabilities through
which evaluation uses the host. `instantiate_default` supplies both the standard
adapter inventory and the standard capabilities for the `testapi/*` vocabulary.
Both answer `Loaded | Diag` — instantiation itself fails informatively when a
host requirement is missing or mismatched, or when a top-level binding's
evaluation goes stuck.

A **host-constructed callback** needs two matching pieces: its exact descriptor
and application-group sizes in the `Host_adapter` inventory, and an arm in the
separately supplied `call` capability that implements it. The host extends the
standard inventory, wraps the standard dispatch with arms for its own closures,
instantiates with both, and mints a guest value for each with
`hostfn_in(descriptor)` — the guest applies it like any function. The mint fails
immediately with a diagnostic if the adapter did not advertise that exact
descriptor; a successfully minted value fires only after receiving the runtime
values in every declared application group. The callback set is fixed per
instantiation; the case study's callback section shows the whole pattern.

```kio {ignore}
// `im` is the loaded image from the load step.
match! instantiate_default(im) {
  .(handle: Loaded) { /* open the existential and call exports */ };
  .(d: Diag) { /* a host requirement or top-level binding failed: report why */ }
}
```

## Public newtype members are exports too

A guest may export a nominal newtype without exporting either member. Each
member becomes a callable export only when both the outer newtype and that
member are plain `pub`. Given a bridged guest module declaring both members
public:

```text
pub newtype Tag[A] : A { pub constructor mk_tag; pub projector un_tag }
```

the surface carries `guest.Tag.mk_tag` and `guest.Tag.un_tag` (bare:
`Tag.mk_tag` / `Tag.un_tag`), looked up and applied exactly like fn
exports, listed by `exports` with their canonical signatures, and
statable in a contract. A payload spelled through the image's own `type`
aliases renders structurally, so the listed and contract-matched
signature never names a type the host cannot see:

```kio {ignore}
let c = contract_cons(
  , mk_expect("Tag.mk_tag"(String), "[#0] (#0) -> n{guest.Tag}(#0)"(String))
  , contract_cons(
    , mk_expect("guest.Tag.un_tag"(String), "[#0] (n{guest.Tag}(#0)) -> #0"(String))
    , contract_nil()
    )
  );
```

At the erased runtime a constructor and a non-existential projector are
the identity — `un_tag` applied to `mk_tag`'s result rides the payload
through unchanged. An existential newtype's projector follows Kio''s CPS
elimination ([`specs/prime.md` § The `newtype`
primitive](../../specs/prime.md#the-newtype-primitive)): apply it to the
sealed value and then to a continuation, which receives the opened
payload — two `apply` steps through the surface, the continuation minted
with `hostfn_in` or taken from another loaded value.

## Write the typed wrapper once

The surface is deliberately representation-hiding, so a raw call is a
little ceremony: look the export up, marshal the arguments in, apply,
marshal the result out, and thread the `Diag` cases. A host does not write
that ceremony at every call site — it wraps the surface **once per
package** in its own typed interface, and that wrapper type-checks at the
host's own compile time, which is what makes runtime loading safe.

The case study's worked example does exactly this. Its `greet` wrapper is
four lines of ceremony behind a typed signature — written once, called
everywhere:

```text
fn run_greet[P](surf: Surface(P), who: String) -> String {
  match! surf.?{lookup}("greet"(String)) {
    .(g: P) {
      match! surf.?{apply}((g, surf.?{str_in}(who))) {
        .(r: P) {
          match! surf.?{str_out}(r) {
            .(s: String) { s };
            .(d: Diag) { diag_message(d) }
          }
        };
        .(d: Diag) { diag_message(d) }
      }
    };
    .(d: Diag) { diag_message(d) }
  }
}
```

Everything the wrapper touches is the surface record — it is generic in
`P`, so it never learns the representation; it only promises its caller
`String -> String` semantics over the loaded package. A wrapper for a
different export shape swaps the marshalling pair (`i32_in` / `i32_out`,
`pair_in` for a product-domain export, `sum_is_left` / `sum_payload` for a
sum-returning one) and keeps the same skeleton.

## Where to go next

- The [`dyn_load_prime` case study](../poc/dyn_load_prime.md) is the
  complete, runnable read-through: a host loads a freshly-emitted guest
  image, contract-matches it, and calls every export shape through the
  surface, with the diagnostics shown end to end.
- [Debugging with Kio'](debugging-with-kio-prime.md) covers the
  `kio-prime` target from the diagnostics angle — reading the lowered form
  the loader consumes.
- [`specs/prime.md`](../../specs/prime.md) is the Kio' core the image is
  written in; [`specs/package.md`](../../specs/package.md) specifies the
  `bridge { … }` contract surface a host contract-matches against.

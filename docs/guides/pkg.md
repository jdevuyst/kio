# Package files and bridges

Every Kio package has one package file at its root: `<pkg>.pkg.kio`.
A package — Kio's hosted compilation unit — is a named collection of local
Kio module trees plus that file. The package file declares build
configuration and a single `bridge { ... }` block that selects which of the
package's modules form its contract with the host.

A package is not a module. A module is a namespace and typechecking unit.
The capabilities a package needs from its host, and the entries it offers
back, are declared as ordinary items inside modules — not in the package file.
The package file only names, with module globs, which modules participate in
the host boundary.

The authoritative contract is [`specs/package.md`](../../specs/package.md);
this guide is the narrative companion.

## The package file

A package file begins with `package <name>;`, where `<name>` matches
the filename stem. The optional `build` and `bridge` blocks can follow in
either order. `kio fmt` prints them in this canonical order:

1. `package <name>;`
2. optional `build { ... }`
3. optional `bridge { ... }`

There are no `env`, `export`, or per-root adaptation blocks: the host
boundary is computed from the modules the `bridge` block selects, not
enumerated in the package file.

```kio {variant=package}
// logger.pkg.kio
package logger;

bridge {
  logger;
  logger/**
}
```

## Host declarations live in modules

A module declares what it needs from the host with `host type` and `host fn`
declarations, and what it offers with ordinary `pub` items:

```kio {variant=module}
// logger.kio
module logger;

host type String role(str);

host fn print(s: String) -> .;
```

```kio {ignore}
// logger/main.kio
module logger/main;

import logger(String, print);

pub fn log_impl(s: String) -> . { print(s) }
```

`host type` / `host fn` are signature-only declarations — no body. They are
always public and obey ordinary lexical scoping; another module reaches them
through a plain `import`, like any other item. A `host type` may carry a
`role(...)` annotation (`role(str)`, `role(i32)`, `role(bool)`, …) that ties
the type to a host-supplied atomic value and lets literal tokens be typed
against it. Multiple host types in one scope may carry the same role. A bare
literal needs a unique admitted host identity when no surrounding type selects
one; `if`/`else` and `__if_then_else__` likewise require exactly one in-scope
`role(bool)` identity. Direct host types are selected when present; aliases are
the fallback pool otherwise, with repeated paths to one declaration counted
once. The pool uses unqualified lexical names: a qualified module import alone
does not add its members, though an in-scope alias may reach its terminal host
identity through one.

A role-bearing host type is always zero-arity: type parameters and
`role(...)` cannot appear on the same declaration. Generic host types such as
collections remain roleless, and their parameters are ordinary types such as
`[T]`; a host type cannot bind a higher-kinded parameter such as `[*F]`.

Roleless host types and host functions can be generic. A host-owned collection
type, for example, is written as a parametric `host type`, and host functions
can bind the same type parameters:

```kio {variant=module}
// arrays.kio
module arrays;

host type I32 role(i32);

host type String role(str);

host type Array[T];

host fn array_make_empty[T]() -> Array(T);

host fn array_push[T](a: Array(T), v: T) -> .;

host fn array_get[T](a: Array(T), i: I32) -> T;
```

A generic host type is still opaque: Kio knows `Array(String)` and
`Array(I32)` are applications of the host type family, but the array
representation and mutation semantics belong to the host functions. When a
generic host function has no value argument from which to infer a type
parameter, pass the type argument and its Unit value at the call site, such as
`array_make_empty(String, ())`. When value arguments determine it, ordinary
inference is enough, as in `array_push(names, "ada")`. Any ordinary type
expression can fill the generic slot, including named aliases introduced by
`labels`: a `labels Item = { sku : String, count : I32 };` declaration can be
used as `Array(Item)` and `array_make_empty(Item, ())`.

### Mutable host arrays and `rec(loop)`

Opaque host values are still ordinary Kio values. If a host function mutates
one, allocate it in Kio, pass the same value through a loop-driven walk, and
return it when the walk is done. For most program code, write the walk as a
`rec(loop)` function; see [Recursion](recursion.md) for the
recursion form.

```kio {variant=module}
module array_fill;

import control(if);

host type I32 role(i32);

host type Bool role(bool);

host type Array[T];

host fn leq_i32(p0: I32, p1: I32) -> Bool;

host fn add_i32(p0: I32, p1: I32) -> I32;

host fn array_make_filled[T](n: I32, fill: T) -> Array(T);

host fn array_set[T](a: Array(T), i: I32, v: T) -> .;

host fn loop[S][R](step: S -> S | R, state: S) -> R;

rec(loop) fn fill_scores(i: I32, scores: Array(I32)) -> Array(I32) {
  if! leq_i32(i, 2) {
    array_set(scores, i, add_i32(i, 10));
    rec fill_scores(add_i32(i, 1), scores)
  } else {
    scores
  }
}

pub fn build_scores() -> Array(I32) { let scores = array_make_filled(3, 0); fill_scores(0, scores) }
```

Host helpers are just ordinary function declarations, so text-processing
helpers live at the host boundary too. A package that wants to parse a simple
command language usually declares small inspection primitives and writes the
parser in Kio:

```kio {variant=module}
module text_tools;

host type String role(str);

host type I32 role(i32);

host type Bool role(bool);

host fn string_eq(p0: String, p1: String) -> Bool;

host fn string_len(p0: String) -> I32;

host fn string_slice(p0: String, p1: I32, p2: I32) -> String;

host fn string_to_int(p0: String) -> I32 | .;

fn starts_with_add(line: String) -> Bool { string_eq(string_slice(line, 0, 4), "add ") }

fn add_payload(line: String) -> String { string_slice(line, 4, string_len(line)) }

fn add_amount(line: String) -> I32 | . { string_to_int(add_payload(line)) }
```

There is no ambient tokenizer or string standard library in Kio. Choose the
host functions your package needs, declare them in modules, expose those modules
through the package bridge, and keep the command grammar itself in ordinary Kio
code.

## The bridge block

The `bridge { ... }` block is a `;`-separated list of module paths, spelled the
same way an `import` statement spells them and extended with `*` and `**` shell-glob
wildcards. `*` matches a single path segment; `**` matches a whole subtree.

```kio {variant=package}
package app;

bridge {
  app;
  app/**;
  util/**
}
```

The bridge selects **modules**, and the host boundary is derived from them: from
each matched module, every `host type` / `host fn` item becomes a host
requirement (the env — what the host must supply), every non-host public
callable becomes a host-invocable entry (an export — what the host may call),
and public type declarations contribute the type surface those callables use.
Adding a `host fn` or a `pub fn` to a matched module grows the corresponding
surface automatically; there is no item-by-item ledger to keep in step.

Entries keep their module namespace, so `a/main.main` and `b/main.main` are
distinct host entries even though both leaf names are `main`. There is no export
rename: an entry is named by its declaration under its module path.

## Modules and the contract surface

Regular modules do not import the package file — the package file selects them.
A module declares its own host needs and public items; the package decides which
modules' surface to expose by listing them in `bridge`. A module that no glob
matches is internal to the package and never reaches the host.

Three well-formedness rules keep the exposed contract self-contained:

- **Module completeness.** If a bridged module reaches a `host` item in another
  module, that other module must also be matched by some glob — otherwise the
  generated interface would hide a requirement the host has to satisfy.
- **Type closure.** Every named type reached through a host requirement or
  callable export signature must itself be exposed (its module matched and the
  type plain `pub`), recursively through transparent aliases and compound
  types. A public alias exposes its body. A public newtype exposes only its
  nominal identity until a public constructor or projector exposes the payload
  in that member's signature. This keeps the interface self-contained without
  turning an opaque newtype into its payload.
- **Duplicate export.** Two bridged exports may not collide on the same
  module-qualified path. Entries keep their module namespace, so such a
  collision would require duplicate same-leaf declarations in one module.
  Name resolution rejects those declarations before the bridge contract is
  checked, as an ordinary duplicate-name error rather than a bridge error.

A glob that matches no module is rejected as an almost-certain typo.

## Dependency files

A package can reuse another package through a package-root `<local>.dep.kio`
file. The filename stem and the `dependency` header are the dependency's local
name inside the consumer. The `source` block names where the dependency comes
from, in **exactly one** of two forms — a local path or a remote git repository.

A **local path** dependency points at the dependency's package file with a
relative `/`-separated `path`:

```kio {variant=dependency}
// elab.dep.kio
dependency elab;

source {
  path "../libs/elab/elab.pkg.kio"
}
```

A **git** dependency names a clone URL and a `ref` — a branch, a tag, or a
commit SHA. Add `path` when the repository contains several packages or the
package is nested:

```kio {variant=dependency}
// elab.dep.kio
dependency elab;

source {
  git "https://example.com/libraries.git";
  ref "main";
  path "packages/elab/elab.pkg.kio"
}
```

Here `path` is relative to the Git checkout, not the consumer. It names an
existing package file inside that checkout, not a directory to search; symlinks
cannot select a manifest outside it. Use `/` separators and a relative path.
If the selected file is missing or invalid, fetching fails rather than choosing
another package. Without `path`, Kio requires exactly one package file at the
repository root or one directory below it. The standalone local-path form above
keeps its consumer-relative meaning.

Either way, the dependency's modules are re-rooted under the local name, so a
module that was `module match;` in the dependency is imported as `elab/match` by
the consumer:

```kio {ignore}
import elab/match(match);
import elab/spine_elaborators as spine;
```

Importing from a dependency and exposing a package boundary are separate
steps. The consumer's `bridge { ... }` still controls the host contract, and
the same module-completeness and type-closure rules apply to re-rooted
dependency modules. If a bridged consumer module exposes a signature that
reaches a dependency module's types, or if the dependency module's host
declarations must be part of the consumer's host-facing surface, match the
re-rooted module in the consumer bridge just as you would for a local module:

```kio {variant=package}
package app;

bridge {
  app;
  app/**;
  elab/testapi
}
```

The dependency file makes `elab/...` importable; the bridge entry makes the
needed `elab/testapi` host-facing declarations visible at the package
boundary.

### Pinning a git dependency

A git dependency is **fetched and pinned** so its `ref` resolves to one
reproducible commit. On the first resolve, the exact commit and a digest of the
dependency's contract surface at that commit are written to a `<local>.lock.kio`
file beside the `<local>.dep.kio`:

```kio {variant=lock}
// elab.lock.kio
lock elab;

resolved {
  git "https://example.com/libraries.git";
  ref "main";
  path "packages/elab/elab.pkg.kio";
  commit "0123456789abcdef0123456789abcdef01234567";
  sig "3f9c1e…"
}
```

The lock file is **committed to version control** — it is the pin. Once it is
present, resolution checks out the locked commit and never re-resolves the
`ref`, so a floating `ref` like `main` still produces a reproducible build. The
materialized module tree is committed alongside it; see
[§ The materialization model](#the-materialization-model) below.

The lock records the optional manifest path as well as the URL and ref. Adding,
removing, or changing that path makes the old lock stale; run `kio dep update`
to adopt the new declaration, or restore it. A pathless Git dependency has no
`path` in its lock. Two aliases can select different packages from the same
repository and commit: they share the checkout cache but retain separate locks
and materialized module trees.

Three subcommands manage these dependencies:

- **`kio dep fetch`** materializes the declared dependencies on their own,
  without a build — cloning each git dependency into a per-user cache, writing
  the lock on its first resolve, and re-rooting its modules under the
  consumer's package root. An existing lock is honored, so `fetch` never moves a
  pin; it only regenerates the materialized tree. A dependency whose
  materialized tree already matches the lock is skipped as a no-op (reported
  `up to date`), so re-running `fetch` on an unchanged package does no redundant
  work; pass `--force` to re-materialize unconditionally.
- **`kio dep update`** re-pins git dependencies: it re-resolves each floating
  `ref` to the commit it designates *now*, rewrites the lock to that commit
  (and a fresh contract digest), and re-materializes the tree. This is the
  operation that advances a branch or tag to its current commit; a `path`
  dependency has no lock to move but is re-materialized from its current
  modules.
- **`kio dep clean`** removes the materialized tree from the working directory,
  leaving the `<local>.dep.kio` and any `<local>.lock.kio` untouched. Because
  the tree is committed, this dirties the working tree; `kio dep fetch` (or
  `git restore`) puts it back. Use it to force a clean re-materialization,
  not to keep a tree out of version control.

`kio build`, `kio check`, and `kio test` consume the committed materialized
tree as ordinary source, so they need no fetch step on a fresh checkout.
`kio dep fetch` regenerates the tree (when a dependency source has changed, or
after `kio dep clean`); `kio dep update` is the deliberate step that moves a pin
forward.

### The update honesty gate

Re-pinning can adopt a contract-surface change in the dependency, so
`kio dep update` gates the move on whether that change is **compatible**. It
compares the dependency's contract surface at the old commit against the new
commit's, using the same compatibility relation `kio sig` uses, and:

- a **compatible** move rewrites the lock and succeeds;
- a **breaking** move against a dependency whose contract is **sealed** (it
  ships a committed `<pkg>.sig.kio` changelog) is an **error**, and the lock is
  left unchanged so you stay pinned to the reproducible old commit — pass
  `--allow-breaking` to downgrade the error to a warning and proceed anyway;
- a **breaking** move against an **unsealed** dependency is a **warning**, and
  the re-pin proceeds.

So an honest sealed dependency cannot silently feed you an incompatible surface
through a floating `ref`; you either get a compatible move or an explicit,
opt-in break. The lock's `sig` is what makes the gate work: it records the
contract surface that was pinned, so re-pinning compares the new commit's
surface against it rather than trusting the move blindly.

For the full contract, see
[`specs/package.md` § Dependency files](../../specs/package.md#dependency-files),
[`specs/cli.md` § `kio dep`](../../specs/cli.md#kio-dep-subcommand), and
[`specs/versioning.md` § Git-dependency contract gate](../../specs/versioning.md#git-dependency-contract-gate).

### The materialization model

Materializing a dependency re-roots its module tree under the local name and
writes those `<local>/…` modules to disk beneath the consumer's package root.
That tree is **committed to version control** — tracked in git, not gitignored —
right alongside the `<local>.dep.kio` and any `<local>.lock.kio`. A consumer
therefore checks in its dependency's whole **materialized closure**: when a
dependency declares its own dependencies, the consumer commits those nested,
already-materialized trees too, all nested under the one `<local>/` root.

Two things follow from committing the closure:

- **A fresh checkout is self-contained.** Every dependency module is already on
  disk, so `kio build`, `kio check`, and `kio test` find what they need without
  a fetch step. Cloning the consumer's repository is enough to build it.
- **Resolution never recurses.** Because the closure is already present,
  materializing a dependency is a single, non-recursive step — `kio dep fetch`
  reads the dependency's committed source and re-roots it; it does **not** walk
  into the dependency's own `*.dep.kio` and fetch *its* dependencies. Recursive
  fetch is disallowed by design. A dependency author publishes a package whose
  closure is already committed; a consumer materializes from that committed
  source.

The committed tree is **canonical**: it is byte-for-byte what `kio dep fetch`
regenerates, because materialization pretty-prints each re-rooted module. A
shared dependency therefore produces identical re-rooted bytes across every
consumer that vendors it; content-addressed version control stores those bytes
once, so the committed closures cost little history.

These two properties — committed and canonical — are checked. A
`dep-materialization` gate asserts that every package declaring a dependency
commits the re-rooted tree (with no `.gitignore` hiding it), and a
`dep-canonical` check regenerates the tree with `kio dep fetch --force` (the
`--force` bypasses the up-to-date skip, so the regeneration is a genuine fresh
fetch rather than a no-op) and asserts the committed bytes match what fetch
produces, with no stale, missing, or extra module. The two together keep
"committed" and "what fetch would write" the same thing. Test cases re-fetch
before they run, so the run always exercises the latest materialization rather
than a stale committed copy.

### Reconciling a dependency diamond with `retype`

Cross-package nominal types are **identity-exact**: a `newtype` is identified by
its `(module, name)` pair, so the *same-named* `newtype` reached through two
different dependency paths is two **distinct** types. This is the property that
makes a Kio package boundary honest — a type named `Tag` in one package is never
silently the same as a `Tag` in another (the same module-qualified identity
governs all four type surfaces a signature can reach). But it has a sharp
consequence for **diamond dependencies**.

Suppose a consumer `app` depends on two packages, `core` and `widget`, and each
of them vendors its own copy of a shared `store` module that declares
`newtype Tag : I32`. Re-rooted under the consumer, those become `core/store.Tag`
and `widget/store.Tag` — distinct nominal copies. A value minted as one cannot
flow where the other is expected:

```kio {ignore}
import core/store(make_tag, tag_val);
import widget/relay(relay_tag);   // relay_tag returns widget/store.Tag

// tag_val wants core/store.Tag, relay_tag gives widget/store.Tag — a type error
let n = tag_val(relay_tag(7));
```

The fix is a `retype` clause in the dependency file, which reconciles the
duplicate copies into one:

```kio {variant=dependency}
// widget.dep.kio
dependency widget;

source {
  path "../widget/widget.pkg.kio"
}

retype widget/store to core/store;
```

`retype <local>/<mod> to <to>/<mod>;` names a re-rooted dependency module and a
counterpart module — another dependency's module, or one of the consumer's own —
that declares the same-named `newtype`s. During materialization, every `newtype`
the named module declares is rebound to the counterpart. Public and scoped
heads become ordinary type aliases at their original declaration positions,
using a qualified provider import; private heads use a selective import instead.
Each spelling is introduced once, and the dependency's constructors, projectors, and payload
references all bind to the one shared nominal type instead of the dependency's
own copy. A trailing `.<Name>` on **both** sides restricts the remap to one
`newtype` (`retype widget/store.Tag to core/store.Tag;`); omitting it remaps
every `newtype` under the module. Each exact source newtype may be selected by
only one statement: duplicate statements and module/per-type overlaps are
rejected before dependency output is written, even if they name the same
target. Separate per-type statements for distinct names are valid. The
counterpart must declare a same-named
`newtype` with the same universal and existential parameter counts, the same
effective kind at each corresponding binder, and a structurally congruent
payload. A missing or incongruent counterpart is a dependency error reported
at materialization time, not a downstream typecheck failure.

Retyping also covers the types generated by explicit `labels` entries. For
example, `retype widget/store.Name to core/store.Name;` shares a label-generated
`Name` with the corresponding label family in `core/store`. The target must
come from an explicit label declaration, possibly reached through an ordinary
type alias; a similar hand-written newtype is not enough. Materialized source
keeps the lowercase spelling with a
[label-forwarding declaration](products.md#giving-an-existing-label-another-name)
and the public uppercase spelling with an ordinary type alias. Named record or
sum declarations that reused the selected label become ordinary type aliases
too, so later `_` entries do not depend on a removed declaration. Unselected
label declarations keep their own identities, and function bodies are unchanged.

The target must also be visible wherever the source type was visible, and
importable from each dependency module whose import is rewritten. Scoped
`pub(path)` restrictions use ordinary module ancestry; importing a name
privately into a target module does not re-export it. An invisible target is
rejected before replacing or pruning the affected dependency's materialized
modules, so an existing usable tree remains intact. This visibility check also
runs for an up-to-date or forced fetch. It does not roll back other dependency
writes or lockfile changes.

Public and scoped constructors and projectors must also keep their names and
remain accessible to the same modules. A member is only as visible as its
enclosing type: a public constructor in a private type is private, and one in
a scoped type is scoped. Fetch rejects incompatible public or scoped members
before changing the affected dependency tree. Private member differences do
not prevent fetch; `kio check` or `kio build` reports an error if the dependency
still uses a member that the target no longer provides or makes accessible.

The worked end-to-end is the
[`exec_dependency_retype_diamond`](../../test-data/goldens/00_success/exec_dependency_retype_diamond/)
golden: `app` depends on `core` (which declares `Tag`) and `widget` (which
declares its own `Tag` and relays a value through it). Without the `retype`
clause, threading `widget`'s `relay_tag` result into `core`'s `tag_val` is a
type error and the build fails. With `retype widget/store to core/store;` in
`widget.dep.kio`, the two copies collapse onto one, the package builds, and the
shared `Tag` flows end to end.

### Rehosting a dependency's host items with `rehost`

A dependency module can declare `host` items — types and functions its *own*
host was expected to supply. Re-rooted under the consumer, those move to
`<local>/<mod>`, where the consumer's host does not provide them — so they would
surface as extra, unsatisfiable host requirements on the consumer's boundary. A
`rehost` clause rebinds them onto a consumer module that supplies replacements:

```kio {variant=dependency}
// foobar.dep.kio
dependency foobar;

source {
  path "../foobar/foobar.pkg.kio"
}

rehost foobar/io to testapi/io;
```

`rehost <local>/<mod> to <consumer>/<mod>;` names a re-rooted dependency module
and a consumer module that provides replacements. During materialization, the
dependency module's host items are rewritten **in place** to forward to the
consumer: a host *type* becomes a `pub type` alias for the consumer's type, and
a host *function* becomes a forwarding wrapper that calls the consumer's
function. The rebound items keep their names and stay exported, so any module
that imported them keeps its import unchanged — the rewrite is local to the one
dependency module and touches no other. The consumer module must export an item
of a compatible type for each rebound name; a missing or mistyped provider is an
ordinary typecheck error. A dependency file may carry any number of `rehost`
clauses, one per dependency module to rebind.
Repeating a source module is a dependency error even if the target is unchanged.
Fetch rejects the repeated selector before writing that dependency's module
tree. Selectors for different source modules may share a target.

## Scoped visibility with `pub(path)`

A `pub` declaration is visible to the whole package and, when its module is
bridged, to the host. A `pub` may instead carry a **scope restriction**, written
`pub(<module-path>)`, to export only within a named subtree:

```kio {ignore}
module app/internal;

pub(app) newtype Token : I32 { pub constructor mk; pub projector un }
```

`pub(app)` makes `Token` importable from `app` and anything beneath it, and
sealed everywhere else in the package. The path must be a **prefix of the
declaring module's own path**: visibility relaxes only up toward an ancestor,
never sideways to an unrelated module or down to a narrower one, so a definition
is never visible where its own module cannot reach. A non-prefix path is a
compile error.

A scoped definition is strictly narrower than `pub`, so adding one can never
widen an existing import — open-world compilation holds. It also shapes the host
interface: because the host sits outside every module path, a `pub(path)`
definition is **not** exported to the host even when its module is bridged. Use
it for package-internal surface you want to share across a subtree of modules
without offering it to consumers or the host. The scope is admissible wherever
`pub` is a genuine choice, including a `newtype`'s `constructor` / `projector`
members; only `host` declarations cannot be scoped, since they are always
public.

## Sharing a label across declarations

The `labels` form (see [Structural products](products.md) and
[Structural sums](sums.md)) mints a generated `newtype` from each label name and
its payload type. When another named product or sum needs that same nominal
field, write `_` instead of declaring the payload again:

```kio {variant=module}
module shared_labels;

host type String role(str);

host type I32 role(i32);

labels Item = { sku: String, count: I32 };

labels Order = { sku: _, total: I32 };
```

`sku: _` refers to the earlier `Sku`; it neither infers a payload nor creates a
second type. The original declaration must appear earlier in the same module,
and `_` is available only inside a named `labels` declaration. Repeating
`sku: String` is a duplicate declaration error, as are forward, imported, and
qualified attempts to reuse a label. This explicit spelling makes the shared
identity visible without asking the compiler or reader to compare payloads.

Generic reuse repeats the original label's universal binder arity and kinds
using parameters from the enclosing alias. Existential binders remain solely on
the original declaration. Visibility on the later declaration applies to its
alias; it never changes the earlier generated nominal.

## Build blocks

The package file's optional `build { ... }` block sits directly after the
package header:

```kio {variant=package}
package app;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/"
  };

  target rust {
    out "out/rust/"
  }
}

bridge {
  app;
  app/**
}
```

For the full key list, see
[`specs/package.md` § Build target files](../../specs/package.md#build-target-files).

## See also

- [`specs/package.md`](../../specs/package.md) — package files, the bridge
  block, host declarations, source forms, and the build block.
- [`specs/grammar.md` § Package files](../../specs/grammar.md#package-files)
  — grammar productions.

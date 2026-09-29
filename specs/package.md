# Kio packages

A package is Kio's hosted compilation unit: a named collection of local Kio
module trees plus one package file. The package file declares build
configuration and a single `bridge { ... }` block of module-path globs that
selects which of the package's modules form its contract with the host.

A package is not a module. A module is a namespace and typechecking unit. The
capabilities a package needs from its host, and the entries it offers back, are
declared as ordinary items inside modules — `host type` / `host fn`
declarations for what the host must supply, `pub` items for what the host may
call. The package file only names, with module globs, which modules
participate in the host boundary; the env and export surfaces are *derived*
from the matched modules, not enumerated in the package file.

For language syntax and semantics, see [`language.md`](language.md). For the
package-file and module grammar productions, see
[`grammar.md` § Package files](grammar.md#package-files).

## File types

A package directory contains:

- **`<name>.pkg.kio`** — the package file. Exactly one per package. Begins
  with `package <name>;`, where `<name>` is the filename stem. Carries the
  optional `build` block and the optional single `bridge { ... }` block.
- **`*.kio`** (except `<pkg>.sig.kio`, `<local>.dep.kio`, and
  `<local>.lock.kio`, below) — Kio source modules. Contain
  types, functions, operators, elaborators, `equiv` claims, `host type` /
  `host fn` declarations, and other module-body items.
  A root module begins with `module <name>;`. Submodules live in
  separate files (`module root/sub;`).
- **`<pkg>.sig.kio`** — the optional compatibility changelog (at most one per
  package, at the package root), recording the contract surface version by
  version; maintained by `kio sig` (see [§ Compatibility](#compatibility) and
  [`versioning.md`](versioning.md)). Despite the `.kio` suffix it is **not** a
  source module and is excluded from module discovery.
- **`<local>.dep.kio`** — a dependency-declaration file (one per direct
  dependency, at the package root), declaring one cross-package dependency and
  where to fetch it. Like `<pkg>.sig.kio`, despite the `.kio` suffix it is
  **not** a source module and is excluded from module discovery. See
  [§ Dependency files](#dependency-files).
- **`<local>.lock.kio`** — a dependency lock file (one per remote `git`
  dependency, beside its `<local>.dep.kio`), pinning the exact commit the
  dependency's `ref` resolved to. It is committed to version control and, despite
  the `.kio` suffix, is **not** a source module and is excluded from module
  discovery. See [§ Dependency files](#dependency-files).

At the package root, root `*.kio` stems must be unique. Package files are
identified by their `.pkg.kio` suffix and may share a stem with a root module.
Inside subdirectories, module stems only need to be unique among directory
siblings.

## Module-name rules

Two coherence rules constrain a regular module's `module <path>;`
declaration. Both are checked at parse time; a violation is a parse error
(exit code `11`).

1. **Declared name matches filesystem path.** The `/`-separated segments in
   `module <path>;` must equal the file's location relative to the package
   root, with the trailing `.kio` extension stripped.
2. **Package membership is path-derived.** The package root is the nearest
   ancestor containing `<name>.pkg.kio`; a regular module belongs to that
   package. Adding declarations to a module body cannot change another
   module's package membership.

These checks preserve open-world compilation: both are properties of one
file's path and header, not of another module's declarations.

## Package file

The package file starts with its required header, followed by optional `build`
and `bridge` blocks in either order. Its canonical formatting is:

```kio
package app;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/";
  }
}

bridge {
  app;
  app/**;
}
```

The formatter's canonical order is:

1. `package <name>;`
2. optional `build { ... }`
3. optional `bridge { ... }`

A package file declares no host items and no `import` clauses. The host boundary
lives in modules: a module declares what it needs from the host with
`host type` / `host fn` items and what it offers with ordinary `pub` items
(see [`language.md` § The host boundary](language.md#the-host-boundary)). The
package file's only job beyond build configuration is to name, with module
globs, which modules participate.

## Host declarations

A module declares the capabilities it needs from its host with `host type` and
`host fn` items, and what it offers back with ordinary `pub` items:

```kio
module app;

host type String role(str);
host type Token;
host fn print(s: String) -> .;
```

`host type` / `host fn` are signature-only declarations — no body; the host
fills them in at invocation. They are always public (`pub host` / `host pub`
are accepted but redundant) and obey ordinary lexical scoping: a host item's
signature names types brought into scope by `import`, and another module reaches
a host item through a plain `import`. There is no env block, no role-priority or
override system, and no root-to-submodule inheritance — a `host` item is an
ordinary scoped declaration. The full declaration form is in
[`language.md` § The host boundary](language.md#the-host-boundary).

A roleless `host type` may declare ordinary kind-`*` type parameters. It may
not bind a higher-kinded parameter; `host type Wrapper[*F];` is a type error.

### Roles

A `host type` declaration may carry `role(...)` — one of `i8`, `i16`,
`i32`, `i64`, `i128`, `u8`, `u16`, `u32`, `u64`, `u128`, `f32`, `f64`,
`bool`, or `str`.

A role-bearing host type has no type parameters. Parameterized host types are
roleless and their parameters have kind `*`; combining a type-parameter list
with `role(...)` is a type error.

Roles drive literal reception. Multiple `host type`s in one lexical scope may
carry the same role. When neither an explicit annotation nor an expected type
determines a literal's type, tier 3 resolves it only if exactly one in-scope
role-bearing type is admitted for that literal; zero candidates are unresolved
and multiple candidates are ambiguous. `role(bool)` additionally supplies the
condition type for `__if_then_else__` and the reference `if!` elaborator's
branching glue, but only when the
identity-keyed candidate set is a singleton. Directly in-scope host types form
the candidate set when any exist; only otherwise do transparent aliases supply
the fallback set. Repeated paths to one host declaration contribute one
candidate, while distinct declarations contribute distinct candidates even
when their leaf names match. Zero or multiple
candidates make those uses type errors without making the host-type
declarations themselves invalid.

Here, “in scope” means the module's unqualified lexical namespace: local and
selectively imported host types or aliases. A qualified module import alone
does not contribute its members. An in-scope alias may name its terminal host
identity through a qualified import, but only the alias contributes that
identity to the fallback pool.

Host types are opaque inside Kio code. Structural typing does not cross a host
type boundary; values move in or out through host functions.

The optional `{ owned }` block remains accepted for source compatibility. A
`role(str)` declaration may therefore still spell:

```kio
host type Owned_str role(str) { owned };
```

The block is a redundant source-compatibility annotation, not a language-level
operation.
It selects no alternate facade: the Rust backend renders every `role(str)`
value in owned form at every occurrence, whether the block is present or not.
Other backends likewise assign the block no runtime meaning.

## The bridge block

The `bridge { ... }` block is a `;`-separated list of module-path globs —
module paths spelled as in an `import` statement, extended with `*` and `**`
shell-glob wildcards. `*` matches a single path segment; `**` matches a whole
subtree (zero or more segments).

```kio
bridge {
  app;
  app/**;
  util/**
}
```

The bridge selects **modules**, and the host boundary is derived from them:
from each matched module, every `pub host` item becomes a host requirement (the
**env** — what the host must supply), every other public callable becomes a
host-invocable entry (an **export** — what the host may call), and public type
declarations contribute the type surface those callables use. Adding a
`host fn` or a `pub fn` to a matched module grows the corresponding surface
automatically; there is no item-by-item ledger in the package file. Entries
keep their module namespace, so `a/main.main` and `b/main.main` are distinct
host entries even though both leaf names are `main`; there is no export rename.
A module that no glob matches is internal to the package and never reaches the
host.

A plain-public `newtype` publishes its nominal identity, while its constructor
and projector are independent capabilities. For a bridged module, the four
possible member states are:

| Constructor | Projector | Host surface |
| --- | --- | --- |
| private or `pub(path)` | private or `pub(path)` | nominal type only |
| `pub` | private or `pub(path)` | nominal type and constructor |
| private or `pub(path)` | `pub` | nominal type and projector |
| `pub` | `pub` | nominal type, constructor, and projector |

These rows assume the outer newtype is plain `pub`. A private or
`pub(path)`-scoped outer newtype contributes no host capability regardless of
its members' markers: a member's effective visibility is the intersection of
the outer and member visibility. In particular, a nominal-only newtype may
cross other public function signatures without revealing how to construct,
project, or represent its payload.

Three well-formedness rules keep the exposed contract self-contained, and a
fourth catches typos:

- **Module completeness.** If a bridged module reaches a `host` item in another
  module, that other module must also be matched by some glob — otherwise the
  generated interface would hide a requirement the host has to satisfy.
- **Type closure.** Every named type reached through a host requirement or
  callable export signature must itself be exposed (its module matched by a
  glob and the type plain `pub`), recursively through functions, sums,
  products, type arguments, and transparent aliases, down to primitives and
  `__intrinsics__` types (which are exempt). A public transparent alias exposes
  its body, so the body's dependencies are part of this closure. A public
  newtype instead exposes an opaque nominal identity: its payload is part of
  the closure only when a plain-public constructor or projector exposes that
  payload in the member's callable signature. Publishing the outer newtype
  alone, or mentioning that nominal type in another export, does not traverse
  the payload. This is the Rust E0446 analogue — it makes the exposed interface
  stand on its own without breaking nominal opacity, the property the
  dynamic-loading goal wants.
- **Duplicate export.** Two bridged exports may not share the same
  module-qualified path. Because entries keep their module namespace, the only
  way to collide is two same-leaf exports in one module, which is independently
  invalid as a duplicate declaration.
- **Dead glob.** A glob that matches no module is rejected as an almost-certain
  typo.

Dead glob, module completeness, and type closure are reported as bridge errors
(exit code `20` — see [`exit-codes.md`](exit-codes.md)): dead glob and module
completeness at the glob / module-header span, and type closure at the offending
signature. A same-qualified-path duplicate is a same-module duplicate
declaration (exit code `13` when that diagnostic is reported), so a valid
resolved module tree cannot present one to bridge validation.

## Compatibility

The `bridge { ... }` block, together with the host and `pub` items the matched
modules declare, defines a package's **contract surface**: the env items a host
must supply, the exports a host may call, the public nominal identities and
member capabilities, and the transitive closure of every type their exposed
signatures reach. This surface is the unit of compatibility. A newtype's
capabilities come only from the outer and member visibility markers written on
that declaration, so adding an unrelated declaration to any module cannot
alter them. The module-body open-world property does *not* cover changes to the
bridge selection or those visibility markers (see
[`language.md` § Open-world design](language.md#open-world-design)) — changing
what lives on the contract surface is a **versioning event**.

How the surface may evolve, and how a host or downstream consumer decides
whether a newer package is still compatible, is specified in
[`versioning.md`](versioning.md). In brief: the env side is contravariant (a
host requirement may be dropped but not added) and the export side is covariant
(an export may be added but not removed); a retained item whose normalized type
changes, or a removal that strands a type still referenced by a retained
signature, is breaking. A package records its contract surface version by
version in a changelog file at the package root, `<pkg>.sig.kio`, maintained by
the `kio sig` tooling ([`versioning.md` § The `kio sig`
command](versioning.md#the-kio-sig-command)). The changelog is backend-independent
and is a committed build input for any backend that re-emits deprecated host
items (see [`backends/README.md` § Deprecated host
items](backends/README.md#deprecated-host-items)).

The same contract surface is what a host **contract-matches at load time**
when it loads the package dynamically rather than linking it at build time:
the surface a build-time host links against and the surface a runtime host
checks an image against are the same `bridge`-derived surface. Before any guest
binding is evaluated, every required host type and function is matched by exact
declaring-module and local-name identity against the host adapter's offered
inventory; type descriptors retain parameter kinds or role metadata, and
function descriptors retain their canonical resolved signatures. Canonical
signatures expand transparent aliases, alpha-normalize binders, and fully
qualify nominal and host identities. Each function requirement pairs that
descriptor with the ordered number of runtime values in each right-spine
application group, derived directly from the resolved type shape rather than
reparsed from the canonical signature text; a whole unit group contributes
zero values. The group sequence is nonempty and every count is nonnegative; an
invalid binding is a diagnostic when required-adapter preflight or an explicit
callback lookup makes it authoritative. Unrelated extra offerings are accepted.
A missing requirement, a same-identity descriptor
mismatch, or a function group-shape mismatch is an instantiation diagnostic.
Adding an unrelated declaration cannot retarget an existing requirement;
adding a host declaration to the selected contract surface deliberately expands
that versioned requirement set. Loading
trusts the image's function bodies exactly as build-time linking trusts a
compiled artifact — the loader does not run the Kio' typechecker. The emitted
image is therefore trusted precompiled input rather than source validated by
the loader; successful loading is not a body-typing judgment. The
`<pkg>.sig.kio` changelog is a versioning record, not a load-time input.
Runtime loading is delivered by an ordinary Kio package rather than a
language feature — worked through in the [`dyn_load_prime` case
study](../docs/poc/dyn_load_prime.md) and [Loading Kio' packages at
runtime](../docs/guides/dynamic-loading.md).

## Dependency files

A package depends on another package by declaring it in a dependency file at the
package root. Each direct dependency gets its own `<local>.dep.kio` file; the
filename stem is the dependency's **local name**. Like `<pkg>.sig.kio`, a
dependency file carries the `.kio` extension but is not a source module and is
excluded from module discovery. For the file grammar, see
[`grammar.md` § Dependency file](grammar.md#dependency-file-depkio).

```kio
// foobar.dep.kio
dependency foobar;

source {
  path "../foobar/foobar.pkg.kio"
}
```

**The local name is a consumer-chosen alias.** The stem (`foobar` above, matched
by the `dependency <local>;` header) is the name *this* package gives the
dependency, not necessarily the dependency's own package name. The consumer
picks it, so two dependencies that happen to share an internal package name can
still be told apart by distinct local names.

**The `source` block names where the dependency comes from.** Two source forms
are admitted, and a `source` block declares **exactly one** of them:

- A local **`path`**: a relative, `/`-separated filesystem path (it may climb
  `../`) naming the dependency's `*.pkg.kio` file, resolved against the
  consumer's package root. A path that resolves to anything other than an
  existing `*.pkg.kio` file is a dependency error (exit code `30` — see
  [`exit-codes.md`](exit-codes.md)).
- A **`git`** + **`ref`** pair, optionally with a repository-relative **`path`**:
  `git "<url>";` names a git clone URL
  (`file://`, `https://`, `git@…`, …) and `ref "<rev>"` names a single revision
  designator that git resolves uniformly — a branch name, a tag, or a commit SHA.
  Both keys are required; a `git` without a `ref` (or a `ref` without a `git`) is
  a parse error. An optional `path` selects an exact package manifest inside
  that checkout. All present fields are unique strings and may occur in any order.

```kio
// foobar.dep.kio — one package in a git repository
dependency foobar;

source {
  git "https://example.com/foobar.git";
  ref "main";
  path "packages/foobar/foobar.pkg.kio"
}
```

**A git dependency is fetched and pinned.** Resolving a `git` source clones the
repository into a per-user cache (`$KIO_CACHE_HOME`, falling back to
`$XDG_CACHE_HOME/kio` and then `$HOME/.cache/kio`), content-addressed by the URL,
checks out the commit the `ref` resolves to, and locates the `*.pkg.kio` in the
checked-out tree — which then re-roots exactly like a local `path` dependency.
Without `path`, discovery requires a unique package manifest among files at
the checkout root and one directory level below it. With `path`, only that
manifest is selected: a missing or invalid target never falls back to discovery.
The selector uses relative `/`-separated syntax; rooted, drive-qualified, and
backslash-separated spellings are parse errors (exit code `11`). The target
must resolve to an existing `*.pkg.kio` file inside the checkout, not a directory.
Containment is checked after resolving symlinks, without changing their meaning
by first collapsing `..`; an escaped, missing, or wrong-kind target is a
dependency error at the selector, before writing its first lock or materialized
tree. Ordinary `.` and within-checkout `..` components are admitted.

The clone is reused and fetched on subsequent builds, never re-cloned. A failure
to clone, resolve the ref, or find a single `*.pkg.kio` is a dependency error
(exit code `30`).

**The lock file pins the resolved commit and the dependency's contract digest.**
On the first resolve of a `git` dependency, the exact commit the `ref` resolved
to — and the dependency's **contract-surface digest** at that commit — are
recorded in a `<local>.lock.kio` file beside the `<local>.dep.kio` (same stem):

```kio
// foobar.lock.kio
lock foobar;

resolved {
  git "https://example.com/foobar.git";
  ref "main";
  path "packages/foobar/foobar.pkg.kio";
  commit "0123456789abcdef0123456789abcdef01234567";
  sig "3f9c1e…"
}
```

When the lock file is present, resolution checks out the **locked commit** and
never re-resolves the `ref` — so a floating ref (`main`) still produces a
reproducible build. The lock file is **committed to version control** (it is the
pin), as is the materialized module tree it gates (the consumer commits its
dependency's materialized closure — see *Materialization is committed* below). A
lock file whose recorded `git` URL, `ref`, or optional `path` no longer matches
the `<local>.dep.kio` is stale — a dependency error; use `kio dep update` to
adopt the declaration, or restore it. The exact declared path is part of source
identity: adding, removing, or changing it makes a pin stale, even if the same
manifest or commit would result. Pathless declarations keep pathless locks.
The repository cache remains keyed by URL and each checkout by commit; multiple
aliases selecting different packages share that checkout but have independent
locks, contract digests, and materialized module trees.

The `sig` is a canonical hash of the dependency's contract surface at the pinned
commit — its **sealed** contract when the dependency ships a `<pkg>.sig.kio`
(see [`versioning.md`](versioning.md)), or its bridge-reachable live surface
when it does not. It is what lets `kio dep update` tell whether advancing a
floating `ref` would adopt a contract-surface change, and whether that change is
compatible (see [`cli.md` § `kio dep update`](cli.md#kio-dep-update) and
[`versioning.md` § Git-dependency contract gate](versioning.md#git-dependency-contract-gate)).

**A dependency is a re-rooted package.** The dependency is an ordinary Kio
package. Depending on it means resolving its `*.pkg.kio`, then **re-rooting its
module tree under the local name**: the local name becomes a synthetic leading
module segment, so the dependency's `module app;` is reachable as `foobar/app`,
its `module app/io;` as `foobar/app/io`, and so on. The whole dependency tree
nests beneath the one `foobar` root, which keeps every dependency namespaced —
two dependencies, or a dependency and a local module, can never collide on a
module path.

**Importing from a dependency.** The dependency's `pub` items are importable
under the re-rooted path, with the ordinary `import` forms:

```kio
import foobar/app(greet);
import foobar/app as fb;
```

**Rehosting a dependency's host items.** A dependency module may declare `host`
items — types and functions the *host* must supply. Re-rooted under the
consumer, those items move to `<local>/<mod>`, where the consumer's host (its
bridge env, or a test runner's protocol) does not supply them — so they would
surface as extra, unsatisfiable host requirements. A `rehost` statement rebinds
them onto a consumer module that provides replacements:

```kio
// foobar.dep.kio
dependency foobar;

source {
  path "../foobar/foobar.pkg.kio"
}

rehost foobar/io to testapi/io;
```

`rehost <local>/<mod> to <consumer>/<mod>;` names a re-rooted dependency module
(`<local>/<mod>`, with the dependency's local name as its leading segment) and a
consumer module providing replacements. During materialization, the rewrite is
**local to the named module**: every `host` item it declares is replaced in
place by an ordinary item that forwards to the consumer module. A `host type` T
becomes a transparent alias `pub type T = <consumer>.T` (the consumer type's
`role(...)`, if any, is inherited by the alias, so a literal the host type
received still resolves through it); a `host fn` f becomes a forwarding wrapper
`pub fn f(..) -> R { <consumer>.f(..) }`. The rebound items stay exported, so a
module that imports them keeps its import unchanged — the rewrite touches no
other module. The consumer module must export an item of a compatible type for
each rebound name; a missing or mistyped provider is an ordinary typecheck
error. A dependency file may carry any number of `rehost` statements — one per
dependency module to rebind; `kio fmt` sorts them after the `source` block.
Any second selector for the same source module is a dependency error, even
when both statements name the same target. It is rejected before that
dependency's materialized tree is written; selectors for distinct source
modules remain valid.

**Retyping a dependency's newtypes.** Nominal types are **identity-exact**:
a `newtype` is identified by its `(module, name)`, so two re-rooted copies of
the same `newtype` — the same type reached through two different dependency
paths (a diamond) — are *distinct* types and cannot mix. A `retype` statement
collapses one copy onto another, the `newtype` analogue of `rehost`:

```kio
// app.dep.kio — `widget` re-exports a `Tag` it shares with `core`
dependency widget;

source {
  path "../widget/widget.pkg.kio"
}

retype widget/store to core/store;
```

`retype <local>/<mod> to <to>/<mod>;` names a re-rooted dependency module
(`<local>/<mod>`, with the dependency's local name as its leading segment) and a
counterpart module — another dependency's module or one of the consumer's own —
that declares the same-named `newtype`s. During materialization, every `newtype`
the named dependency module declares is removed and rebound to the counterpart's,
so the dependency's code (its constructors, projectors, and payload references)
binds to the one shared nominal type instead of its own re-rooted copy. The
rebinding **preserves the export surface**: a `newtype` that was `pub` stays
importable from the module that originally declared it (every `pub` item is
importable — see *No sealing* below), re-exported as a transparent alias to the
counterpart so it keeps the shared identity. Public and scoped heads are
replaced by ordinary positional type aliases at their original declaration
slots, through a fresh qualified counterpart import; no selective import
duplicates those local bindings. Private selected heads are removed and
selectively imported instead. Residual recursive groups are partitioned after
these replacements, preserving the scope needed by retained declarations.
A **per-type** form restricts the
remap to a single `newtype`, written with a trailing `.<Name>` on **both** sides:
`retype <local>/<mod>.Tag to <to>/<mod>.Tag;`. Omitting `.<Name>` remaps every
`newtype` under the module. When the counterpart module is **itself** a `retype`
source — a chain `retype A to B; retype B to C;` — the remap reconciles to the
chain's origin (`A`'s `newtype` rebinds to `C`'s, the module that still declares
it), so chained remaps collapse a multi-hop diamond onto one shared type.

Explicit `labels` entries also declare selectable nominal types. Retyping
their generated names requires corresponding explicit label-generated types,
not ordinary newtypes with coincidentally matching payloads and member names.
An ordinary positional identity alias may lead to that explicit counterpart;
the selected module need not separately export its lowercase label spelling.
Materialized source preserves each selected label spelling with a nonminting
`type {label} = {provider.label};` declaration referring to the exact terminal
label family, alongside the ordinary uppercase alias or private import.
Both the selected uppercase head and the terminal forwarding edge retain
their ordinary access obligations.

An affected named `labels` owner becomes an ordinary product/sum alias. This
includes later named owners whose written `_` markers reused one of the
selected earlier explicit labels. Each affected owner is decomposed once at
its original position: unselected explicit entries remain declarations there,
and references to selected entries use the shared nominal. Remaining recursive
members use the ordinary minimal recursive partition; standalone forwards
stay outside those groups. Other declarations and value bodies are not
rewritten for this decomposition. The `_` rule itself is unchanged: a forward
is not an explicit minting origin. Fresh materialized source derives identity
from these declarations and imports, without a record of removed declarations.

The statements form a **partial function from exact source newtypes**. A
module-form statement first expands over the newtypes its source module
actually declares; no later module-form or per-type statement may select any
of those exact `(module, name)` identities again. Exact duplicates,
same-target repetitions, and module/per-type overlaps are all dependency
errors, independent of statement order. The materializer diagnoses the later
selector together with the first before writing the affected dependency tree.
Disjoint per-type selectors from one module remain valid.

The counterpart must preserve the source declaration's importable surface
under ordinary visibility, including `pub(path)` ancestry. Each selective
import introduced or redirected by the remap must name an ordinarily
importable declaration at that materialized use; a private lexical import in
the target module does not export the imported name. Qualified consumers keep
access through the source module's visibility-preserving alias. These
visibility obligations are checked before writing or pruning the affected
dependency's module tree, including forced and already-up-to-date fetches.
Visibility failure is a dependency error (exit code `30`). This guarantee is
specific to the retype interface; it does not make dependency commands atomic
across other validation failures, other dependencies, or lockfile updates.

Each public or scoped constructor and projector must keep its name on the
counterpart's corresponding role, with visibility covering the source member's
effective visibility. A member's effective visibility is the intersection of
its own visibility and its enclosing newtype's visibility: a public member of
a private newtype remains private, and a public member of a scoped newtype is
scoped. These interface failures are also dependency errors (exit code `30`)
checked before writing or pruning the affected tree. Private member names and
access are not checked by fetch; when a retained use becomes invalid, ordinary
`kio check` or `kio build` diagnoses it.

Each remap is checked at materialization: the counterpart must declare a
**same-named** `newtype` (`from`'s newtypes ⊆ `to`'s) with the **same
type-parameter shape**: universal and existential binder counts both match,
and corresponding binders have the same effective kind. A `Box[A]` is not
interchangeable with either `Box[A][B]` or `Box[*F]`, because its use sites
apply a different arity or kind. The payload must be **structurally
congruent** — the outer nominal
boundary is unwrapped and the payloads are compared modulo module paths. A
**host-type** leaf matches its same-named counterpart by name (a host primitive
is the same however it is re-exported / rehosted); any other **nominal** leaf —
itself a `newtype` / `labels` type / `type` alias — must match by **identity**
(its declaring `(module, name)`, following transparent aliases and re-imports to
the type it resolves to), unless this same `retype` is also collapsing it (then
the two sides' same-named copies match by redirect). The structural constructors
`&` / `|` / `->` / `.` / `!` — and a `[..]` binder, whose bound variables and
effective kinds align positionally — must align. A missing counterpart, a
parameter-shape mismatch, or an incongruent payload is a dependency error
(exit code `30`) — a clean
materialize-time diagnostic, not a downstream typecheck failure. A dependency
file may carry any number of pairwise-disjoint `retype` statements; `kio fmt`
sorts them after the `rehost` statements.

**Open-world collision rule.** A dependency only *adds* an importable
`<local>/…` module root; it never changes the meaning of an existing module. The
one hazard is the local name colliding with one of the consumer's own root
modules, which would make `import <local>/…;` ambiguous. So **a dependency's local
name must not equal the first path segment of any of the consumer package's own
modules**; a collision is a dependency error (exit code `30`). Given that rule,
`import <local>/…;` resolves unambiguously — a first segment that names a declared
dependency resolves in that dependency's re-rooted tree, otherwise it resolves
in the consumer's own modules. The open-world argument is stated in
[`language.md` § Open-world design](language.md#open-world-design).

**Materialization is committed.** `kio dep fetch` / `kio dep update` write the
re-rooted `<local>/…` module tree under the consumer's package root, and that
tree is **committed to version control** alongside the `<local>.dep.kio` and any
`<local>.lock.kio`. A consumer therefore ships its dependency's **materialized
closure**: when a dependency itself declares dependencies, the consumer commits
the dependency's already-materialized nested trees too, re-rooted under the one
`<local>/` root. Two consequences follow. First, a fresh checkout is
**self-contained** — `kio build` / `kio check` / `kio test` find every dependency
module already on disk, with no fetch step required. Second, resolving a
dependency never recurses into the dependency's own `*.dep.kio`: the closure is
already present, so a single non-recursive materialization suffices (see
the *Transitive dependencies* boundary below). The committed tree is canonical —
byte-for-byte what `kio dep fetch` regenerates, since materialization
pretty-prints each re-rooted module — so a shared dependency yields identical
re-rooted module bytes across every consumer, which content-addressed version
control stores once. `kio dep clean` removes the working-tree copy (dirtying the
tree) only to force a clean re-materialization; the committed tree is the source
of truth.

**Boundaries.** A dependency is **direct** — resolved in a single,
non-recursive step — from a local `path` or a remote `git` source:

- **No archive source.** The `path` and `git` source forms are the only
  dependency sources.
- **Transitive dependencies: materialized-only.** A dependency that declares its
  own `*.dep.kio` is accepted; its **committed** `<local>/…` closure (it ships its
  own materialized dependency trees — see *Materialization is committed* above)
  re-roots along with it. The consumer materializes the dependency from its
  committed source; it does **not** recurse into the dependency's own
  declarations, so resolving a dependency is a single, non-recursive step. A
  nested dependency the dependency author did not commit surfaces as an
  ordinary unresolved-module error, and diamond/shared resolution is never
  automatic: a diamond's two same-named-`newtype` copies are collapsed
  **manually** with a `retype` statement (see *Retyping a dependency's
  newtypes* above).
- **No sealing.** Every `pub` item in every dependency module is importable by
  the consumer; a non-bridged dependency module is present and open, and the
  dependency's bridge exports place no restriction on the consumer-visible
  surface.
- **Host items rebind only through `rehost`.** A dependency's `host` items are
  merged into the consumer package as ordinary module items; an explicit
  `rehost` statement (see *Rehosting a dependency's host items* above) is the
  only mechanism that rebinds a dependency module's host items onto a consumer
  provider. Nothing folds a dependency's residual env requirements into the
  consumer's contract surface implicitly — an unrebound host item stays a host
  requirement the consumer's host must supply.

## Build target files

The package file's optional `build { ... }` block configures compilation
targets. It may precede or follow `bridge` after `package <name>;`.

```kio
package mypkg;

build {
  cache "out/.kio-cache/";

  docs {
    md "docs";
    support "poc-elaborators";
    md_out "out/docs-md/";
    html "out/docs/"
  };

  target js {
    out "out/js/"
  };

  target rust {
    out "out/rust/";
    namespace "my_kiopkg"
  }
}
```

`cache` is optional. A string path enables package caches under that
directory; `()` disables caching. When absent, it defaults to `cache ();`.

Entries inside `build`, `docs`, and each `target` block may be written in any
order. Singular entries cannot be duplicated. Repeated `target` and `support`
entries retain their relative order; [`style.md`](style.md) specifies canonical
placement of the different entry kinds.

`docs` is optional and points `kio doc` at the package documentation tree.
`md` is required when the docs block is present; `support` is repeatable;
`md_out` and `html` are optional, naming the Markdown and HTML output
directories and defaulting to `out/docs-md/` and `out/docs/` respectively.

Each `target <id> { ... }` block names one backend. The `out` key is
**required** in every target block and names the target's output directory;
it is the universally meaningful target key, and other keys are
backend-specific. `out` must be a path **relative** to the package root —
an absolute path is rejected as a build error.

The build block is local to the package. A package file with no build block
declares no compilation targets, and `kio build` exits at the build-error
category.

### Per-target keys

- **`namespace`** *(every backend)* — overrides the per-package
  namespace the emitted artifact's top-level names live under: the
  crate name (and the root the branded facade names derive from) on
  rust, the `package` clause on go, the `package` declaration on java,
  the module stem on python, the artifact stem (the `<ns>.js` /
  `<ns>.d.ts` file name, which also roots the branded facade names) on
  js and ts, the Swift module name (the `// kio-swift-module:` marker)
  on swift, and the module name `<Ns>` on haskell. Defaults to a per-backend
  derivation of the package name;
  each backend page's § Output layout gives the derivation and the
  accepted value grammar, and a value outside that grammar is a build
  error.
- **`thread_safety`** *(rust backend)* — opts the emitted crate into a
  thread-safe value representation. Accepted values are `"send"`,
  `"sync"`, and `"send_sync"`.

Per-backend output contracts live in [`backends/`](backends/).

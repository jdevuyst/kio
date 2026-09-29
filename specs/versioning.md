# Package versioning

This document specifies how a Kio package's **contract surface** evolves
across versions: the compatibility relation a host or consumer enforces when
loading a package, the changelog artifact (`<pkg>.sig.kio`) that records the
surface version by version, and the `kio sig` tooling that classifies a change
as backward-compatible before it ships.

A package's contract surface is everything reachable from the items its
`bridge { ... }` block exposes (see [`package.md` § The bridge
block](package.md#the-bridge-block)): the `host` items it requires of the host
(the **env** side), the other `pub` items it offers back (the **export**
side), and the transitive closure of every type those signatures reach.
Open-world compilation is deliberately scoped to *module bodies* and does not
cover this surface (see [`language.md` § Open-world
design](language.md#open-world-design)); changing the surface is a **versioning
event**, and this document is its contract.

## The compatibility relation

A package's contract surface is morally `(host satisfying env) -> exports`. The
two sides carry opposite variance, and the whole compatibility relation follows
from that:

- The **env** side is **contravariant**. The host *supplies* these items, so a
  newer package may ask for *less* but not *more*: dropping a host requirement
  is safe (an existing host already supplies a superset), but adding one
  breaks every host that didn't anticipate it.
- The **export** side is **covariant**. The package *provides* these items, so
  a newer package may offer *more* but not *less*: adding an export is safe,
  but removing one breaks every caller that used it.

Identity is **module-qualified**: an item in module `a` and an item of the same
leaf name in module `b` are distinct (`a.main` ≠ `b.main`). The relation
compares the two surfaces structurally over normalized Kio' types; it never
consults a version *number*. A version number is changelog bookkeeping (see
§ The version header) — the loader and the compatibility check are both purely
structural.

### Export side (provisions, covariant)

| Change | Verdict |
|---|---|
| Export removed | **breaking** |
| Retained export's normalized type is not alpha-and-kind-equal to the baseline (return narrows, parameter widens, …) | **breaking** |
| Retained `pure fn` export loses `pure` | **breaking** |
| Retained `fn` export gains `pure` | compatible |
| `newtype` export: a `pub` member becomes private, or a `pub` member's signature changes | **breaking** |
| Export added | compatible |
| A private newtype member's payload changes, with no `pub` member exposing it | invisible (not a surface change) |

### Env side (requirements, contravariant)

| Change | Verdict |
|---|---|
| Host declaration added (`host fn` / `host type`, or a newly bridged host-bearing module) | **breaking** |
| Retained host declaration's type is not alpha-and-kind-equal to the baseline, **or its `role(...)` changed** | **breaking** |
| Host declaration removed | compatible (an existing host already supplies a superset) |

A `host type`'s `role(...)` is part of its identity: changing `role(str)` to
`role(bool)` is a breaking env change. The changelog records each host type's
role.

#### The side flip

A declaration can change *which side* of the contract surface it lives on. The
common case is turning a `host fn` (env, a host requirement) into a `pub fn`
(export, a package provision) — for example, the package grows an
implementation for a capability it previously asked the host to supply. The
changelog treats a side flip as a single **breaking** change on the one
qualified name that moved: the flip reverses the host-provides / package-provides
*direction* for that name, so a consumer keyed on it must change code regardless
of which way it moved.

The *abandoned* host-side `fn` is additionally handled like a removed `host fn`
for source stability (§ Deprecated host items): a backend that re-emits removed
host items re-emits the abandoned host-side `fn` as a deprecated stub, so an
existing host that still implements it keeps compiling. That source-stability
re-emit does not soften the verdict — the contract change itself is breaking,
and a side flip is not a way to drop a host requirement without the removal-side
obligation.

### Per-kind type identity

Both tables compare *normalized Kio' types*, and the closure walk that reaches
every type a signature mentions compares each type by its kind:

- **transparent alias** — inlined; a named export records its name plus its
  expansion.
- **`newtype`** (including `labels`-generated newtypes) — compared by qualified
  name plus kind and arity; the payload is reached only through the newtype's
  `pub` members.
- **structural** (`&`, `|`) — compared structurally.
- **host `type`** — compared by qualified name plus type parameters plus role;
  opaque (its payload is not part of its identity).
- **polymorphic** — compared by alpha-equivalence, with binder kinds matching.

### Strictness

The compatibility relation is **strict alpha-and-kind-equality** on retained
items: any retained item whose normalized type is not alpha-and-kind-equal to
its baseline is breaking. This is sound (it never reports a breaking change as
compatible) at the cost of over-reporting a pure generalization — replacing
`Int -> Int` with `[A] A -> A` is reported breaking even though every old call
site still typechecks. The changelog format does not bake this conservatism
in: a change is filed under `breaking` or `nonbreaking` by its *computed
verdict* (see § Classification is verdict-based), so verdict precision is not
part of the format contract.

Exported function purity is a capability rather than part of the normalized
function type. Adding `pure` lets existing consumers keep using the function
and additionally makes it available to compile-time evaluation, so it is
compatible. Removing `pure` takes that existing use away and is breaking.

### Removal that strands a referenced type

Removing a host item is compatible by the env table **only if the removed item
is not reachable from any retained bridged signature**. A removal that leaves a
retained signature referencing a now-unexposed type would produce an ill-formed
interface — the same failure the type-closure rule catches for a single version
(see [`package.md` § The bridge block](package.md#the-bridge-block)). So the
compatibility check re-runs contract-surface well-formedness (module
completeness and type closure) over the *replayed* interface after a removal,
and classifies a closure-breaking removal as **breaking**.

### Two relations the design keeps distinct

The compatibility relation above answers one question: *does an old host still
satisfy the new package?* That is the language-level contract, enforced
structurally by the loader. A second, backend-level question — *does the
regenerated host-side interface still compile against unchanged host source?* — is
**source stability**, and it does not always agree with the first. Removing a
`host fn` is compatible by the env table, yet it breaks an existing Rust
`impl Host` (an orphan trait method). The per-backend deprecated re-emit
(§ Deprecated host items) addresses source stability without weakening the
language relation; the two are specified separately because a change can be
compatible by one and not the other.

That per-package source-stability question is distinct again from a backend's
[`Host API stability`](backends/README.md#host-api-stability). The backend
status compares regeneration across Kio or emitter upgrades while package
source, target configuration, and the documented package contract stay
unchanged. An intentional package-contract change recorded by `kio sig` remains
governed by this document and does not become a backend compatibility break.

## The signature artifact: `<pkg>.sig.kio`

A package records its contract surface in one canonical file at the package
root: `<pkg>.sig.kio`. The file **is** an append-only changelog — a sequence of
per-version blocks. The current interface is not stored as a standalone
section; it is *derived by replaying* the blocks in version order.

Properties:

- **Backend-independent.** The recorded signatures are typed Kio' signatures.
  The source-compatible `{ owned }` block on a `host type`, and any other
  non-contract annotation, is **rejected**: the block selects no alternate
  facade and does not belong in the changelog's backend-independent contract
  surface. Each backend reads the same changelog and applies its own
  deprecation policy to that contract, never to source-only annotations.
- **History-bearing.** The changelog remembers what was removed: a `remove`
  block names an item, and the item's signature lives in the earlier
  `add`/`modify` block that introduced it. The file is therefore *not* a
  recompute-from-source snapshot — the history is what enables the deprecated
  re-emit (§ Deprecated host items). For a fixed changelog plus source, `kio sig
  stage` is idempotent: re-running it produces byte-identical output, so the
  file is diffable and CI-gateable.
- **A build input.** A backend that re-emits deprecated items (§ Deprecated host
  items) reads the changelog at build time, so build output is a function of
  *both* the source and the changelog. Both are committed to the package, like
  a lockfile; the changelog is a declared, committed build input, not a hidden
  side channel. An implementation that caches build artifacts must fold the
  changelog's content hash into the artifact cache key — otherwise a changelog
  edit silently serves a stale artifact.
- **Excluded from `*.kio` walks.** The artifact's name ends in `.kio` because
  its contents *are* Kio' syntax, but it is a changelog, not a module. Every
  command that walks a package's `*.kio` files (check, build, fmt, doc, …)
  excludes `<pkg>.sig.kio`; treating it as a module would fail the
  module-name-coherence rules ([`package.md` § Module-Name
  Rules](package.md#module-name-rules)).
- **No stored metadata.** The changelog records *what changed at each version*
  and an optional commit message (§ The version-block doc slot) — nothing more.
  In particular it stores **no seal date / time**: git already records when a
  version's commit landed, so the date stays recoverable from `git log` rather
  than being duplicated in the file. It also carries **no tamper hash** or
  integrity checksum: git history plus `kio sig status`'s structural drift
  detection already distinguish an honest edit from a stale changelog, so a hash
  would add ceremony without catching anything those two miss.

### Replay

The current interface is the result of replaying every block in version order:
an `add` introduces an item, a `modify` reshapes a currently live item, and a
`remove` drops it. A removed name can become live again only through `add`;
its retained historical declaration does not make it a valid `modify` target. The
*active* env and export surfaces — what the compatibility relation compares,
and what a host implements against — are the replay's result, never a stored
duplicate.

For a version with `with`, replay first validates that complete version-local
declaration context, advances unchanged peers to the new context epoch, and
then applies `breaking` followed by `nonbreaking`, with `add`, `modify`, and
`remove` in their fixed order. An add/modify FQN must resolve exactly in that
version's `with`; no later context or ambient source declaration may satisfy
it. A supporting public declaration whose recorded shape differs from the
currently live item must itself be named by `modify`, and a newly introduced
one by `add`, so `with` cannot smuggle an operation past the per-name ledger.

History is collapsed only by an explicit `compact` (§ History growth and
`compact`); there is no garbage collection. A removed item's introducing block
must survive so that a later `remove` can recover the item's side (host vs.
export) and its frozen signature.

## Changelog grammar

The `<pkg>.sig.kio` file has its own file shape, reusing the existing Kio'
declaration grammar at the *section* level. It is parsed by the changelog
grammar, not as a single regular module.

### The version header

The file begins with a header naming the package and its current contract
generation:

```text
signature app v(3);
```

`signature` is a file-shape keyword, a sibling of `package` and `module`.
`v(N)` is the current generation — the number `kio sig commit` increments. The
generation is **author, CI, and changelog bookkeeping only**: neither the
loader nor the compatibility check ever reads it (both are structural). There
is no format-version or schema-version field.

### Version blocks

The body is a sequence of per-version blocks ordered **oldest-first**. An
ordinary changelog begins with `v(1)` directly under the header and has no
generation gaps. A changelog may instead begin at a later generation only when
that first block has the self-validating compact-boundary shape — correctly
partitioned `add` entries only, with each qualified name introduced once — and
every surviving block after it remains contiguous. The boundary may carry an
optional `///` version message; the message is inert during replay, which keeps
the shape closed under `uncommit` followed by a message-bearing `commit`. The
parser derives the later-starting authority from the block itself, never from a
claim that a particular tool produced it. No other later-starting history is
admitted. A nonempty history ends at the header generation, or at the generation
immediately before it when the open draft block is absent. The order is
presentational; the tooling replays by version number.

Each version block admits an optional version-local `with` declaration context
and optional `breaking` and `nonbreaking` sections, in any input order.
Canonical formatting prints `with`, then `breaking`, then `nonbreaking`,
omitting empty sections:

```text
signature app v(2);

v(1) {
  nonbreaking {
    add {
      module app {
        host type Str role(str);
        host fn print(p0: Str) -> .;
      }
    }
  }
}

v(2) {
  nonbreaking {
    add {
      module app {
        pub fn greet(p0: Str) -> .;
      }
    }
  }
}
```

The **presence of a `breaking { ... }` section is the version's breaking-ness**
— there is no separate breaking-or-not marker. Inside each partition, changes
are `add` / `modify` / `remove` blocks.

In schematic EBNF (the declaration productions are defined below):

```ebnf
VersionBlock       ::= DocBlock? 'v' '(' INT_LIT ')' '{' SemiEntries<VersionEntry>? '}' ';'?
VersionEntry       ::= WithBlock | BreakingBlock | NonbreakingBlock
WithBlock          ::= 'with' '{' SemiEntries<SigModuleSection> '}'
BreakingBlock      ::= 'breaking' '{' SemiEntries<SigOperation> '}'
NonbreakingBlock   ::= 'nonbreaking' '{' SemiEntries<SigOperation> '}'
SigOperation       ::= ('add' | 'modify') '{' SemiEntries<SigOperationEntry> '}'
                     | 'remove' '{' SemiEntries<SigRemoveEntry> '}'
SigOperationEntry  ::= SigItemRef | SigModuleSection
SigRemoveEntry     ::= SigItemRef | LegacyRemoveSection
SigItemRef         ::= ModulePath '.' IDENT
SigModuleSection   ::= 'module' ModulePath '{' SemiEntries<SigModuleEntry> '}'
SigModuleEntry     ::= ImportEntry | DocBlock? SigDeclaration
SigDeclaration    ::= TypeAliasBody | Newtype | RecursiveNewtype | TypeRecGroup
                     | HostTypeBody | HostFnBody | SigExportFn
LegacyRemoveSection ::= 'module' ModulePath '{' SemiEntries<IDENT> '}'
```

`SemiEntries` and the declaration/import bodies are defined in
[`grammar.md`](grammar.md). Every nested sequence owns its single semicolon
separators, including between two braced children. It permits an optional
leading or trailing separator, not repeated runs. Imports precede declarations
within a module section; at least one declaration is required. Declaration
restrictions for each section remain as specified below. Only the outer
`v(N)` block accepts a redundant suffix after its closing brace; a nested
section's following semicolon belongs to its parent sequence. The signature
header retains its terminator.

The `with`, `breaking`, and `nonbreaking` entries may occur in any order and
at most once each. Within a partition, `add`, `modify`, and `remove` blocks
likewise admit any order, at most once each. Input ordering does not change
the fixed semantic [replay order](#replay); canonical presentation is specified
in [`style.md`](style.md).

`SigItemRef` is the existing non-expression exact declared-name category; it
is never parsed as an expression. The final dot separates a slash-delimited
module path from one declaration name: `api.A` and `foo/bar.B`.

#### The version-block doc slot

A sealed version block carries an optional `///` doc comment naming the message
the author recorded when sealing it with `kio sig commit -m "..."`:

```text
/// Add the `greet` export; drop the unused `debug` host hook.
v(2) {
  nonbreaking {
    add { module app { pub fn greet(p0: Str) -> . ; } }
  }
}
```

The grammar admits a single `///` doc slot immediately before a `v(N) { ... }`
block — the changelog's version-block analogue of an item doc comment. The slot
is optional (a block sealed without `-m` has none), holds the verbatim commit
message, and is what `kio sig log` prints alongside the version. It is changelog
prose only: neither the loader nor the compatibility check reads it.

#### Classification is verdict-based

A change is placed under `breaking` or `nonbreaking` by its **computed
verdict** against the compatibility relation, not by which operation produced
it. A `remove` of an export is breaking; an `add` of an export is nonbreaking;
a `modify` is breaking or nonbreaking depending on whether the new type is
compatible. The placement is *verified* against the variance rules, so a
changelog that files a breaking change under `nonbreaking` is rejected. (The
partition is keyed to the verdict, not the operation.)

### Operation entries and version-local context

An ordinary non-grouped `add` or `modify` entry remains an inline module
section:

```text
add {
  module util/io {
    host type Str role(str);
    host fn write(p0: Str) -> .;
  }
}
```

Each module section carries its own Kio' `import` entries, separated from the
following entry by the module section's semicolon, so a cross-module reference
resolves at *its* version even if a
referenced type later changes. The braces make each module's boundary explicit
within the nested block structure.

A recursive declaration group is different: the complete group is one
self-validating scope, while compatibility operations and verdicts remain
per declaration name. Every such group therefore appears exactly once in the
version-local `with` context, including in `v(1)` and when all of its members
share one operation and verdict. The operation blocks name changed members by
exact FQN and never repeat the declarations or module scaffolding:

```text
v(1) {
  with {
    module api {
      rec {
        pub type Tree = Branch;
        pub newtype Branch : Tree {
          pub constructor branch;
          pub projector un_branch;
        };
      }
    }
  }
  nonbreaking {
    add {
      api.Branch;
      api.Tree;
    }
  }
}
```

`with` is declaration context, not another operation partition. It may contain
complete recursive groups and, when a previous group splits or becomes
acyclic, the minimal standalone declarations needed to establish the new
context epoch. Every public declaration introduced or structurally changed by
that context must still have its own `add` or `modify` reference under the
computed verdict. An unchanged peer appears only in `with`: it receives the
new complete context without falsely recording a per-name change. A removal is
named directly and needs no declaration in `with`, though the minimal new
context of surviving peers accompanies the version when removing the member
changes their group.

Each `with` module section carries the exact `import` clauses needed by its
retained declarations. Its names are version-local: replay never imports a
group from a later version or reconstructs one from ambient module
declarations. A fresh Kio' parse validates the complete group, including one
genuine SCC, an acyclic alias-only subgraph, strict positivity, and ordinary
import resolution. An operation FQN selects only the exact declaration in
that context; it cannot resolve through an expression, local alias, or
unqualified candidate search.

When a recursive component changes, the new `with` block is a new immutable
context epoch. Each active name retains both its own last `add`/`modify` origin
and its latest complete recursive context. Merge, split, removal, and re-add
therefore do not conflate per-name history with group identity. Projection
through the public host surface recomputes SCCs after private payload facts are
erased; if the projected declarations are acyclic, the changelog omits a
redundant group and records ordinary inline declarations instead.

### Declarations are Kio'

Declarations inside a module section are post-elaboration **Kio'**:

- `host type` / `host fn` — already signature-only in Kio'.
- `type`, `newtype`, `rec newtype`, and a complete `rec { ... }` type group
  (the elaborated forms; never surface `labels`). A mutual group is admitted
  only in `with`, not inline under an operation.
- **Body-less `fn` signatures** for exports — a changelog-specific production
  that reuses the host-fn signature shape:

  ```text
  pub pure fn greet(p0: Str) -> Str ;
  ```

  This is distinct from a regular-module `fn`, which requires a body. A stray
  body on a changelog `fn` is a changelog-context error. The optional `pure`
  modifier records whether the exported ordinary function is available to
  compile-time evaluation. No other signature-file declaration admits
  `pure`: host functions are runtime requirements, and `type` / `newtype`
  declarations have no purity.

No surface-only forms (`elab`, `op`, `match`, `do`, …) appear — they are not
Kio'.

A `newtype` records its elaborated form with its members. For example, after
`labels` elaboration, a label entry surfaces as the generated newtype:

```text
add {
  module app {
    newtype Old_type : Str {
      pub constructor mk;
      pub projector get;
    };
  }
}
```

(Kio' user type names are `_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*` — an
optional single leading underscore, letter-first words with trailing digits,
single internal separators, and any trailing underscores. Only the first
letter is uppercase. The
`labels` desugar capitalizes the label's first letter, so a label `old_type`
mints the newtype `Old_type`.)

The recorded declaration exposes exactly the newtype's plain-`pub` member
surface; scoped `pub(path)` is not exported into a signature. A constructor or
projector that is not plain `pub` is replaced by the private placeholder
`sig_private_constructor` or `sig_private_projector`; if that spelling would
collide with a retained plain-`pub` member, the smallest positive `_nN` suffix
that does not collide is appended. The placeholder never preserves the hidden
source member's name. When neither member is plain `pub`, the payload is
recorded as `.` and existential parameters are omitted, so the artifact cannot
reveal a fully opaque payload. Documentation, trivia, and `import` clauses needed
only by omitted private facts are likewise absent. Such a recorded newtype
replays as opaque; the signature section itself implies the declaration's
outer exported visibility.

### Removal records names only

A `remove` block lists names, not signatures:

```text
remove {
  app.Old_type;
  app.old_fn;
}
```

Canonical output uses exact `ModulePath.IDENT` references for every removal.
The legacy `module app { old_fn; }` spelling remains accepted when reading an
existing changelog but formats to `app.old_fn;`; new output never writes the
nested form. A bare name remains side-ambiguous on its own — a value-shaped name
could be a host fn or an exported fn, and a type-shaped name a `host type` or an exported
`newtype` / `type` — so the changelog verifier **replays the history to the
item's introducing `add` / `modify` operation** to recover its side (which
selects the partition) and its frozen signature (which the deprecated re-emit
consumes). This is the reason replay history must not be garbage-collected: a
removed item's origin must survive for its side and signature to be
recoverable. A later re-add creates a new active origin and, for a recursive
group member, a new explicit context epoch.

### Module removal is an empty module

There is no dedicated module-removal operation. A module is removed by removing
all of its items; after replay the module contributes nothing, and an empty
module is admissible. The strands-a-referenced-type re-check (§ Removal that
strands a referenced type) still applies — a module removal that leaves a
retained signature dangling is breaking.

### Coexisting type versions

A module section's per-block `import` resolves cross-module *names* within that
block. It does **not** carry two live *definitions* of one nominal type. When a
nominal newtype is superseded by a later `modify`, a deprecated re-emit of its
old shape is carried by the **frozen definition in its introducing block**, not
by a use-alias to a second live definition.

## The `kio sig` command

`kio sig` records and classifies contract-surface changes. A version is a
mutable **draft** until `kio sig commit` **seals** it; the compatibility check
is always against the **last sealed version**, never the open draft.

Bare **`kio sig`** is **non-mutating**: it prints the status summary for the
package — the same compatibility state `kio sig status` gates on. On a package
that has no `<pkg>.sig.kio` yet it prints a friendly discoverability line
instead:

```text
kio sig: `app` has no compatibility changelog yet — `kio sig commit` seals the first version
```

so the first-time path is reachable without reading the docs. (There is no
build-time nudge for a sig-less package; discoverability lives in that friendly
line and in this spec — see § Build-time staleness warning.) The mutating work
is in subcommands:

- **`kio sig stage`** — reconcile the source with the current draft, recording a
  **compatible** delta. The draft is **recomputed as the diff between the last
  sealed version and the current source on every run**. This is what makes the
  command idempotent: it collapses a sealed item that was modified and then
  removed into a single `remove`, and it auto-retracts a previously forced break
  once the source reconciles back to the sealed shape. A bare `kio sig stage`
  writes only into the draft's `nonbreaking` partition (the dual of `--force`'s
  explicit `breaking` section, below). It is an **error** only when the change
  breaks the *sealed* contract (an unforced break). Removing or changing
  something that exists only in the current unsealed draft is free — the
  intra-draft add and remove net out — and is reported as a **warning**
  ("`foo` was added then removed within v(N) before sealing"). The warning is
  derived by diffing the previous on-disk draft against the freshly recomputed
  one; it is presentational, not a persisted operation log.

- **`kio sig stage --force`** — record a break against the sealed contract into
  the draft, writing a `breaking { ... }` section. The break stays
  unsealed-pending until a `commit`.

- **`kio sig commit [-m "<message>"]`** — seal the current draft and open the
  next: increment `v(N)` to `v(N+1)`, making the just-sealed version the new
  compatibility baseline. `commit` records no contract change of its own; it
  seals exactly the staged draft. It first requires the worktree to be **fully
  reconciled** and **errors on any unrecorded delta** — compatible *or*
  breaking, not only an unforced break — so a compatible addition cannot be
  silently sealed away and dropped from the changelog. The optional `-m`
  message is stored as the sealed version block's `///` doc slot (§ The
  version-block doc slot) and shown by `kio sig log`.

- **`kio sig uncommit`** — pop the most recent sealed version back into the
  draft (tip-only — it unseals only the newest sealed version, never an earlier
  one). It is **`--force`-gated**: without `--force` it refuses with a note that
  it rewrites an already-sealed contract — a contract other parties may have
  built against — and that a *forward* fix (a new `commit`) is almost always the
  right move; with `--force` it proceeds, decrementing `v(N)` back to the
  unsealed draft state. This is the inverse of `commit`, scoped to the tip so it
  cannot silently rewrite deep history.

- **`kio sig status`** — a git-`status`-like report; the **authoritative CI
  gate**. It exits with a code from the `8x` tier (§ Exit codes). The states
  overlap (a pending forced break also breaks the sealed contract), so the codes
  are assigned by a **precedence order**, not independent tests:

  1. the source breaks the sealed contract **and the break is unrecorded** —
     exit **80** (incompatibility; acknowledge it with `stage --force` or
     reconcile);
  2. else a break **is** recorded but unsealed — exit **82**
     (unsealed-break-pending; `commit` to seal it);
  3. else there is an unrecorded *compatible* delta — exit **81**
     (stale-but-compatible; run `kio sig stage`);
  4. else — exit **0**.

  The inversion in (2) is load-bearing: a *recorded* break reports **82**, not
  **80**, and a pending break outranks compatible drift, so a `--force`'d break
  never reports clean.

- **`kio sig log [--breaking] [--since <N>]`** — pretty-print the changelog. The
  file *is* the history, so this renders the recorded blocks, each version with
  its `-m` commit message (its `///` doc slot). `--breaking` filters the display to
  versions that carry a `breaking { ... }` section; `--since <N>` scopes the
  display to versions after `N`. Both are **read-only display filters** — they
  change what is shown, never the file. A filtered block retains the minimal
  `with` context required to validate and replay the retained operation names,
  closing through both the context epoch before the version and the one the
  version installs. This keeps unchanged peers needed by a merge or split but
  drops unrelated groups and declarations; the filtered text never shows a
  dangling FQN or misleading partial recursive scope.

The write-path commands (`kio sig stage`, `kio sig stage --force`, `kio sig
commit`, `kio sig uncommit`, `kio sig compact`) accept `--stdout` to print the
would-be changelog instead of writing the file.

### History growth and `compact`

The changelog grows by one block per sealed version and is never automatically
collapsed. The single escape hatch is **`kio sig compact <version>`** — **log
compaction**, not a lossy flatten. It collapses the operation-additive history
*before* `<version>`; a nonempty eligible prefix becomes one synthesized
add-only block at `v(<version> - 1)`. Every item in that prefix must be
introduced exactly once by `add`; a prefix containing `modify`, `remove`, a
same-name re-add/later incarnation, an invalid verdict partition, or any state
that cannot be represented by the synthesized boundary is rejected before the
file is changed. The cut must not exceed the current header generation. Thus an
open draft remains in the suffix at its own generation; it is never folded into
a boundary that could reclassify sealed entries as open breaking changes.

The boundary re-materializes the prefix's effective interface from each
item's frozen declaration and its own origin imports. Distinct historical
module sections are not allowed to blur two different import environments.
Recursive members are re-materialized with their complete active context
epoch in one boundary-leading `with` block and exact per-name add references;
each group appears once even when its members have different historical
origins. Projection revalidates the group after public/private erasure.
Every block at or after `<version>` retains the same semantic history: its
version, operations, declarations, message content, and removal generations do
not change. The complete changelog is emitted canonically, so whitespace and
the presentational ordering of sections, items, `import` clauses, and removals may
be normalized. A suffix `remove` may therefore recover an introduction
collapsed into the boundary while keeping its original removal generation and
complete frozen state. A `remove` before the cut is ineligible; compaction never
moves it to the boundary or resets its generation.

After synthesis, the exact canonically rendered artifact that would be printed
or written must reparse under the narrow later-starting compact-boundary rule
and replay to the same live interface, frozen declarations, origin imports,
retained removals, and removal generations. Both the complete replay and the
replay through `v(header - 1)` — the last sealed baseline — must agree before
and after compaction. Failure of any check leaves the original changelog
unchanged.

### Build-time staleness warning

`kio check` and `kio build` emit an **advisory** warning when a package ships a
`<pkg>.sig.kio` **and** the live source carries an **unrecorded breaking**
delta against the last sealed contract — the same condition `kio sig status`
reports as exit `80`. The warning points at `kio sig` for the details. It is
deliberately narrow and quiet:

- a package with **no** `<pkg>.sig.kio` gets **no** warning (nothing to be stale
  against);
- a merely **nonbreaking** unrecorded drift is **silent** (it is not a
  correctness concern — only `kio sig status` reports it, as exit `81`).

The warning is **advisory and non-load-bearing**: it never changes the build's
exit code or output. This is required by open-world — a package without a sig
builds **identically** to one with a sig, so the changelog can be adopted (or
dropped) without changing what `kio build` emits. `kio sig status` remains the
**authoritative, exit-coded CI gate**; the build-time warning is only a nudge so
an unrecorded break is hard to miss during ordinary iteration.

### Package scope

`kio sig` operates per package. With no argument it fans out over every package
in the current directory — each with its own `<pkg>.sig.kio`, its own
generation, and an independent report / stage / commit (there is no
cross-package rollback). It also accepts explicit package-path arguments to
scope a subset, mirroring `kio build`: an argument that names an existing path
(a directory or a `*.pkg.kio` file) is a **package selector**, not a target id.

## Exit codes

`kio sig status` reports through a dedicated `8x` tier (see
[`exit-codes.md`](exit-codes.md)):

| Code | Meaning |
|---|---|
| 80 | incompatibility — the source breaks the last sealed contract and the break is unrecorded |
| 81 | stale-but-compatible — an unrecorded compatible delta exists |
| 82 | unsealed-break-pending — a break is recorded but not yet sealed by `commit` |

Like the existing command tiers (`5x` test, `6x` fmt, `7x` doc), `8x` is a
**`kio sig status` command tier**, not a compile-diagnostic category. `status`
validates the package before comparing its changelog, so a package that fails
validation reports that diagnostic's code rather than an `8x`. The precedence
among `80` / `81` / `82` is the command-specific decision order in § The
`kio sig` command.

## Deprecated host items

Removing a `host fn` is compatible by the env table, yet it breaks an existing
host's hand-written implementation: the implementation still supplies a method
the regenerated interface no longer declares. To keep existing host source
compiling across a removal, a backend may **re-emit the removed item** only as
an optional deprecated shim. A new host written against the current package
supplies and selects only current items; retained history cannot add a method
implementation, associated-type choice, generic binding, factory argument, or
other removed obligation. This is backend-level source stability (§ Two
relations the design keeps distinct), entirely outside the loader's structural
match.

The mechanism is backend-independent at the changelog level and per-backend at the
emitter level:

- **Signature source.** The removed item's signature comes from its introducing
  `add` / `modify` block in the changelog history (the reason history is not
  garbage-collected). Any structural shape in the signature is re-minted from
  the frozen closure in that block, into the *same* shape-registry space as the
  live emit, so an abandoned shape coincides with an equal live shape rather
  than standing alone.
- **Deprecation window.** *How long* a backend keeps re-emitting a removed item
  is the emitter's policy, read from the `remove` / `add` history. The changelog
  performs no window bookkeeping and no garbage collection.
- **Deprecated provenance.** Every host-visible declaration whose only source
  is retained history carries the target's stable deprecation marker. This
  includes a callable shim and any history-only binding, carrier, alias, shell,
  or support type kept so old source can still name its frozen signature. If
  the same exact declaration is live, live provenance wins and the declaration
  is not deprecated.
- **Optionality and fallback.** A trapping/default body is allowed only when
  the host language makes it a genuine default that a new host need not
  implement or select. If preserving old source would instead force a new host
  to provide any removed item, the backend omits that compatibility surface
  and documents the resulting old-host source edit as a concrete backend
  caveat. Breaking old host compilation is preferable to resurrecting a dead
  supply obligation.
- **Per-backend rendering.** Each backend documents how it re-emits a removed
  host item in its backend page, with a mutual cross-reference back to this
  section. Any case it genuinely cannot keep source-stable requires matching
  concrete-caveat top banners in that backend page and its host guide, plus a
  comment at the exact emitter degradation site citing the detailed backend
  section; that section names the emitter site in turn. The recorded signatures
  stay backend-independent; only the consumption is per-backend. See
  [`backends/README.md` § Deprecated host items](backends/README.md#deprecated-host-items),
  [`backends/rust.md` § Deprecated host items](backends/rust.md#deprecated-host-items),
  and [`backends/js.md` § Deprecated host items](backends/js.md#deprecated-host-items).

A re-emitted removed `host fn` is never *called* by the package — the package
dropped it — so its re-emitted body is unreachable from package code. It does
not enter loader matching, live binding selection, a runtime adapter, package
capabilities, or package dispatch. A backend whose optional re-emit needs a
body therefore supplies a diverging one. Host source can still name and call
that deprecated default, but the call traps; it is not live package
functionality.

## Relationship to the loader and to the bridge contract

The compatibility relation is what a host or a downstream consumer enforces at
load time, and what `kio sig` *reports*. The loader contract-matches the host's
supplied surface against the package's required surface **structurally** — it
never matches on the version number `v(N)`. There is **no solver** in the
bridge or signature path: the relation is the variance-aware, module-qualified
comparison above and nothing more.

The host-shape manifest a backend emits (the serialized host descriptor) is
where a consumer's check reads the host surface; it carries the host types and
host fns the contract requires.

## Git-dependency contract gate

The compatibility relation is also what `kio dep update` consults when advancing
a git dependency's floating `ref` (a branch or tag) to a new commit. A git
dependency's lock file (`<local>.lock.kio`, see [`package.md` § Dependency
files](package.md#dependency-files)) records the dependency's
**contract-surface digest** at the pinned commit, in a `sig "<…>";` field of its
`resolved { … }` block. The digest is a canonical hash of the dependency's
contract surface at that commit:

- **Sealed** — when the dependency ships a `<pkg>.sig.kio`, the digest is over
  its **last sealed** interface (the replay result, the same surface this
  document's compatibility relation compares). The dependency's contract is
  *sealed* iff its changelog has at least one committed version.
- **Unsealed** — when the dependency ships no `<pkg>.sig.kio` (or one still on
  its uncommitted first draft `v(1)`), the digest is over its bridge-reachable
  **live** surface: the `host`/`pub` type, newtype, alias, and function
  **signatures** its bridged modules declare, including `pure` on exported
  ordinary functions. This live surface is projected
  from the dependency's *signatures alone* — function bodies are never lowered
  or typechecked to compute it — so it digests identically in the full and
  standalone Kio' checking pipelines even when the dependency's bodies use
  surface forms (block and blockless elaborator calls) that only the full
  front-end accepts. A
  surface-only declaration that would enter the contract only *after* surface
  elaboration — notably `labels`, which the full front-end lowers to newtypes —
  is **not** part of the unsealed live surface: projecting it would require the
  surface front-end the standalone Kio' checker deliberately omits, so a dependency
  that exposes such a form in its public interface must be **sealed** (ship a
  `<pkg>.sig.kio`) to freeze it into its contract.

The digest reuses the contract-surface projection above, so it is a pure
function of the dependency's contract surface: the same surface digests
identically across implementations and across time, and a digest change is
exactly a contract-surface change.

**The honesty gate.** On `kio dep update`, when the re-pin moves the commit and
a prior pin with the same source identity exists, the dependency's contract surface at the **old** commit is
compared (by this document's compatibility relation) against the **new**
commit's, and the verdict gates the re-pin. Source identity is the URL, ref, and
optional exact declared manifest path. An explicit selector chooses the same
manifest path in both checkouts; it never falls back to discovering a different
package. Changing that identity is a fresh source pin rather than a comparison
between unrelated packages, even when the commit is unchanged:

- **compatible** → the lock is rewritten (new commit + new digest); the update
  succeeds.
- **breaking** on a **sealed** dependency contract → an **error**; the lock is
  left unchanged, so the consumer stays pinned to the reproducible old commit.
  Passing `--allow-breaking` downgrades the error to a warning and proceeds.
- **breaking** on an **unsealed** dependency contract → a **warning**; the lock
  is rewritten.

The error/warning split mirrors the sealed-vs-unsealed precedence `kio sig
status` uses (a break against a *sealed* contract is the load-bearing, blocking
state, the `80`-tier analogue; a break against an *unsealed* draft is advisory,
the `82`-tier analogue), but `kio dep update` is a dependency-tier command, so a
blocked sealed break exits with the dependency code `30`, not an `8x` code (the
`8x` tier is reserved for `kio sig status`). The flag and the gate's command
behavior are specified in [`cli.md` § `kio dep update`](cli.md#kio-dep-update).

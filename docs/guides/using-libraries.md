# Using libraries

Kio libraries are packages consumed through explicit dependency files. There
is no ambient standard library or package registry: a consumer chooses a local
name and a local-path or Git source, materializes the dependency under that
name, and imports its public modules normally.

The reusable packages under [`test-data/poc/`](../../test-data/poc/) are
adopter-grade repository libraries. They are checked as real packages and are
good starting points for applications, but their location does not make them
compiler builtins.

## The minimal dependency workflow

Create one `<local>.dep.kio` file at the consumer package root. For a nearby
checkout, point `path` at the dependency's package file:

```kio {variant=dependency}
// elab.dep.kio
dependency elab;

source {
  path "../libs/elab/elab.pkg.kio"
}
```

For a package inside a Git repository, provide a clone URL, a ref, and the
package manifest's path relative to that repository's root:

```kio {variant=dependency}
// elab.dep.kio
dependency elab;

source {
  git "https://github.com/jdevuyst/kio/";
  ref "main";
  path "test-data/poc/elab/workdir/elab.pkg.kio"
}
```

The filename stem, `dependency` header, and local import root are all `elab`.
The dependency package's original name does not have to match it. The Git
`path` selects that exact package file inside the checkout; a standalone
`source { path "..." }` instead resolves from the consumer's package root.
Git sources without an explicit `path` use package discovery.

From the consumer's package root, where `elab.dep.kio` lives, materialize the
source:

```sh
kio dep fetch
```

For a Git source, the first fetch also writes `elab.lock.kio` with the resolved
commit, selected path, and contract digest. An existing lock is honored;
`kio dep update` is
the explicit operation that moves the pin. Commit the dependency declaration,
the Git lock when present, and the materialized `elab/` module tree. The whole
materialized closure is source, not a disposable build cache, so a fresh
checkout can build without network access.

## Re-rooted imports

Materialization places the dependency's module tree below the consumer-chosen
local name. If the dependency declares `module match;`, a consumer that names
it `elab` imports it as `elab/match`:

```kio {ignore}
import elab/match(match);
import elab/spine_elaborators(widen_sum, one_prod);
```

The bang belongs only at the call site:

```kio {ignore}
fn inject[A][B](value: A) -> A | B { widen_sum!(value, A | B) }
```

Importing a dependency does not automatically expose it through the consumer's
host contract. If a bridged public signature reaches dependency types or host
requirements, include the relevant re-rooted modules in the consumer's
`bridge` block. [Package files and bridges](pkg.md) covers rehosting, retyping,
contract closure, locks, and update compatibility in detail.

## Collection and value libraries

Each package below has a root user-facing module and checked demos. Several use
the host-provided `loop` capability for unbounded traversal because Kio' itself
is strongly normalizing.

- [`dict`](../../test-data/poc/dict/) is an ordered persistent dictionary based
  on a red-black tree. Callers pass an explicit key comparator; ordering is a
  dependency, not inferred from a typeclass.
- [`list`](../../test-data/poc/list/) is a generic cons list with O(1)
  constructors and accessors plus loop-driven folds and transformations.
- [`option`](../../test-data/poc/option/) represents optional values as the
  structural sum `Present(A) | .`, with constructors, folds, mapping, binding,
  and collection helpers.
- [`queue`](../../test-data/poc/queue/) is a persistent FIFO queue using the
  two-list representation, with amortized O(1) enqueue/dequeue behavior and
  explicit invariant helpers.
- [`result`](../../test-data/poc/result/) is a right-biased structural
  `Result(T, E) = T | E` with construction, folding, mapping, binding,
  recovery, and operator vocabulary.
- [`vec`](../../test-data/poc/vec/) is a persistent indexed vector backed by a
  binary digit trie, with logarithmic lookup, update, and append paths.

With a local name matching the package name, typical imports are rooted at its
public module:

```kio {ignore}
import dict/dict(Dict, empty, insert);
import list/core(List, cons, nil);
import option/core(Option, none, some);
import queue/core(Queue, empty);
import queue/queue(enqueue);
import result/result(Result, ok, err);
import vec/vec(Vec, empty, push_back);
```

Select only the names the application uses. The package source and generated
Kiodoc remain the authority for exact signatures.

All six collection and value packages above live in the same Git repository.
For example, create `list.dep.kio` at your consumer package root:

```kio {variant=dependency}
dependency list;

source {
  git "https://github.com/jdevuyst/kio/";
  ref "main";
  path "test-data/poc/list/workdir/list.pkg.kio"
}
```

For any other package in this table, name the file `<local>.dep.kio`, replace
`dependency list;` with `dependency <local>;`, and replace only the `path` value
with its row. The Git URL and ref stay the same; the import column shows the
module root and one useful selection after materialization.

| Local name | Git `path` | Example import |
| --- | --- | --- |
| `dict` | `test-data/poc/dict/workdir/dict.pkg.kio` | `import dict/dict(Dict, empty, insert);` |
| `list` | `test-data/poc/list/workdir/list.pkg.kio` | `import list/core(List, cons, nil);` |
| `option` | `test-data/poc/option/workdir/option.pkg.kio` | `import option/core(Option, none, some);` |
| `queue` | `test-data/poc/queue/workdir/queue.pkg.kio` | `import queue/core(Queue, empty);` |
| `result` | `test-data/poc/result/workdir/result.pkg.kio` | `import result/result(Result, ok, err);` |
| `vec` | `test-data/poc/vec/workdir/vec.pkg.kio` | `import vec/vec(Vec, empty, push_back);` |

Run `kio dep fetch` from the consumer package root after adding each
dependency declaration. This writes its lock and materialized source under the
chosen local name; import those modules as shown. A package's public host
requirements still need the consumer's ordinary bridge/rehosting setup, as
explained in [Package files and bridges](pkg.md).

### A list consumer with host bindings

The `list/core` constructors above use no host operations. Traversal functions
in `list/list`, such as `foldl`, use the library's declared host capabilities.
For example, a consumer can total two prices represented as integer cents.
Use the `list.dep.kio` above and create `invoice.pkg.kio`:

```kio {variant=package}
package invoice;

bridge {
  invoice;
  list/core;
  list/list;
  list/elab/testapi
}
```

Then create `invoice.kio`:

```kio {placeholder={"list/core":"core","list/list":"list"}}
module invoice;

import list/core as lists;
import list/list as traversal;

pub fn subtotal(first: traversal.I32, second: traversal.I32) -> traversal.I32 {
  let prices = lists.cons(first, lists.cons(second, lists.nil()));
  traversal.foldl(traversal.add_i32, 0, prices)
}
```

The bridge exposes the library's host requirements under `list/list`. Supply
these bindings in the host implementation; the names below are relative to
that module:

| Binding | Type or operation |
| --- | --- |
| `Bool`, `I32`, `String` | Host types with roles `bool`, `i32`, and `str` |
| `add_i32`, `sub_i32` | `(I32 & I32) -> I32` |
| `eq_i32`, `lt_i32` | `(I32 & I32) -> Bool` |
| `loop` | `[S][R] ((S -> S \| R) & S) -> R`; continue on the left arm, return the right arm |
| `string_concat` | `(String & String) -> String` |

The elaborator dependency also requires host types under `list/elab/testapi`:
`Bool` (`bool`), `I32` and `Int` (both `i32`), and `String` (`str`).

These are the bridged modules' requirements even though this particular
function only uses addition and iteration. With those bindings, calling
`subtotal(125, 250)` returns `375`. The [host integration guides](../README.md#host-integrations)
show how to supply typed host values and functions in each language. To reuse
bindings already provided by a consumer module, put
`rehost list/list to your_module;` in `list.dep.kio` and bridge that provider;
[rehosting](pkg.md#rehosting-a-dependencys-host-items-with-rehost) explains the
compatible-name and type requirements.

Fetch and check from the directory containing `invoice.pkg.kio`:

```sh
kio dep fetch
kio check
kio test
```

## The elaborator library

[`elab`](../../test-data/poc/elab/) is the reusable compile-time structural
toolkit used throughout these guides. Its modules are deliberately separate so
a consumer can import a narrow vocabulary:

- `spine_elaborators` supplies the everyday `fit!`, `reorder_*`, `narrow_*`,
  `widen_*`, `flatten_*`, and `one_*` product/sum adapters.
- `row_elaborators` constructs, updates, and projects label-product rows.
- `tuple_elaborators` supplies tuple operations such as `head!`, `tail!`,
  `concat!`, `flatten!`, `group!`, `zip!`, and `map!`.
- `match` supplies exhaustive, first-match structural dispatch through
  `match!`.
- `control` supplies lazy `if!` branches and ordinary `scope!` blocks.
- `sequence` supplies `Bind` and `Sequence` types and `do!` blocks that
  sequence actions through an explicitly supplied bind function.
- `lookup` supplies the advanced conditional cross-sum helpers `lookup!` and
  `contains!`. They inspect whether an active sum branch contains a requested
  label; they are not aliases for `.?{field}`, which projects a statically
  known product field.
- `derive` composes an explicit set of candidate rules to build one target
  value through `derive!`.
- `type_of` reflects a requested type into the library's user-level type
  representation.
- `show` synthesizes a string renderer for supported structural shapes.
- `algebraic_elaborators` supplies the DNF-level `iso!`, `into!`, `onto!`,
  `align!`, `ease!`, and `atom!` palette.
- `elaborator_util` contains shared implementation types and folds for
  elaborator authors rather than an application-facing surface.

For example:

```kio {ignore}
import elab/match(match);
import elab/control(if, scope);
import elab/sequence(Bind, Sequence, do);
import elab/lookup(contains, lookup);
import elab/spine_elaborators(fit, widen_sum);
```

[Defining elaborators](elaborators.md) explains the reflection ABI behind
these modules. [The elaborator library case study](../poc/elab.md) walks their
implementations and laws.

## The optics library

[`optics`](../../test-data/poc/optics/) provides lenses, prisms, and
isomorphisms as pure function pairs. It includes composition, `view`, `set`,
`over`, prism preview/review, product lenses, sum prisms, and structural
isomorphisms. It depends on `elab` for the spine adapters, so consuming its
materialized package also consumes the committed dependency closure.

Import the public module through the chosen root:

```kio {ignore}
import optics/optics(Lens, compose_lens, view, set);
```

See [The optics library](../poc/optics.md) for the complete executable tour and
its `equiv` laws.

## Case studies, not drop-in libraries

Two POCs teach integration patterns rather than offering a single reusable
module surface:

- [Higher-kinded types](../poc/hkt.md) assembles kinded brands, explicit
  `Functor`/`Monad` dictionaries, monadic `do`, and `derive!`. Use it as a
  design case study; the focused language mechanics are in
  [Higher-kinded types](higher-kinded-types.md).
- [Dynamic loading](../poc/dyn_load_prime.md) demonstrates a host loading and
  contract-matching an emitted Kio' package. It is an end-to-end loader and
  guest scenario, not a general module to import; start with
  [Dynamic loading](dynamic-loading.md).

## Verifying an adopted package

Run the consumer's normal checks after materialization:

```sh
kio check
kio test
```

`kio test` skips dependency-owned `equiv` declarations by default. Add
`--include-deps` when you intentionally want to discharge the full adopted
closure as well. The materialized tree should stay canonical: if the declared
source changes, regenerate it with `kio dep fetch` (or deliberately re-pin a
Git dependency with `kio dep update`) and commit the resulting source changes.

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
// macros.dep.kio
dep macros;

source {
  path "../libs/macros/macros.pkg.kio"
}
```

For a package inside a Git repository, provide a clone URL, a ref, and the
package manifest's path relative to that repository's root:

```kio {variant=dependency}
// macros.dep.kio
dep macros;

source {
  git "https://github.com/jdevuyst/kio/";
  ref "main";
  path "test-data/poc/macros/workdir/macros.pkg.kio"
}
```

The filename stem, `dep` header, and local import root are all `macros`.
The dependency package's original name does not have to match it. The Git
`path` selects that exact package file inside the checkout; a standalone
`source { path "..." }` instead resolves from the consumer's package root.
Git sources without an explicit `path` use package discovery. `path ()` means
the same thing; neither spelling pins a discovered path.

From the consumer's package root, where `macros.dep.kio` lives, materialize the
source:

```sh
kio dep fetch
```

For a Git source, the first fetch also writes `macros.lock.kio` with the resolved
commit, selected path, and contract digest. An existing lock is honored;
`kio dep update` is
the explicit operation that moves the pin. Commit the dependency declaration,
the Git lock when present, and the materialized `macros/` module tree. The whole
materialized closure is source, not a disposable build cache, so a fresh
checkout can build without network access.

## Re-rooted imports

Materialization places the dependency's module tree below the consumer-chosen
local name. If the dependency declares `mod match;`, a consumer that names
it `macros` imports it as `macros/match`:

```kio {ignore}
import macros/match {macro match}
import macros/spine_macros {macro widen_sum; macro one_prod}
```

Type imports retain the original declaration identity and its literal-admission
role. For example, `import numbers {type Int as Integer; type Float}` introduces those type
identities without making them implicit literal candidates. Explicit annotations or
concrete local context determine carriers; ordinary imports do not create an
ambient role-bearing candidate pool.

Call the selected local name directly:

<!--kio {harness=inject placeholder="__INSERT_CODE_HERE__"}
mod kiodoc;

import macros/spine_macros {macro widen_sum}

__INSERT_CODE_HERE__
-->

```kio {@inject}
fn inject[A][B](value: A) -> A | B { widen_sum(value)[A | B] }
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
  structural sum `Present[A] | .`, with constructors, folds, mapping, binding,
  and collection helpers.
- [`queue`](../../test-data/poc/queue/) is a persistent FIFO queue using the
  two-list representation, with amortized O(1) enqueue/dequeue behavior and
  explicit invariant helpers.
- [`result`](../../test-data/poc/result/) is a right-biased structural
  `Result[T][ E] = T | E` with construction, folding, mapping, binding,
  recovery, and operator vocabulary.
- [`vec`](../../test-data/poc/vec/) is a persistent indexed vector backed by a
  binary digit trie, with logarithmic lookup, update, and append paths.

With a local name matching the package name, typical imports are rooted at its
public module:

```kio {ignore}
import dict/dict {type Dict; fn empty; fn insert}
import list/core {type List; fn cons; fn nil}
import option/core {type Option; fn none; fn some}
import queue/core {type Queue; fn empty}
import queue/queue {fn enqueue}
import result/result {type Result; fn ok; fn err}
import vec/vec {type Vec; fn empty; fn push_back}
```

Select only the names the application uses. The package source and generated
Kiodoc remain the authority for exact signatures.

For a fallback that should run only when needed, select a macro explicitly:
`import option/core {macro or_else_lazy; macro unwrap_or_lazy}`,
`import result/result {macro or_else_lazy; macro unwrap_or_lazy; macro unwrap_err_or_lazy}`,
or `import dict/dict {macro get_or_lazy}`. Import only the names used in a
module; the option and result names coincide, so use an alias if both are needed.
For example, `unwrap_or_lazy(value) { default_value() }` extracts an option's
present value or a result's success, invoking the block only on absence or
error. `or_else_lazy` returns an alternative of the same option or result type.
Result's `unwrap_err_or_lazy` extracts the error or invokes the block on
success. `get_or_lazy(compare, dictionary, key) { default_value() }` returns
the dictionary value or invokes the block when the key is absent. The default
must have the extracted value or error type. These calls evaluate their input
and thunk-producing expression once; only the selected fallback body runs.
The corresponding eager functions and operators remain available.

All six collection and value packages above live in the same Git repository.
For example, create `list.dep.kio` at your consumer package root:

```kio {variant=dependency}
dep list;

source {
  git "https://github.com/jdevuyst/kio/";
  ref "main";
  path "test-data/poc/list/workdir/list.pkg.kio"
}
```

For any other package in this table, name the file `<local>.dep.kio`, replace
`dep list;` with `dep <local>;`, and replace only the `path` value
with its row. The Git URL and ref stay the same; the import column shows the
module root and one useful selection after materialization.

| Local name | Git `path` | Example import |
| --- | --- | --- |
| `dict` | `test-data/poc/dict/workdir/dict.pkg.kio` | `import dict/dict {type Dict; fn empty; fn insert}` |
| `list` | `test-data/poc/list/workdir/list.pkg.kio` | `import list/core {type List; fn cons; fn nil}` |
| `option` | `test-data/poc/option/workdir/option.pkg.kio` | `import option/core {type Option; fn none; fn some}` |
| `queue` | `test-data/poc/queue/workdir/queue.pkg.kio` | `import queue/core {type Queue; fn empty}` |
| `result` | `test-data/poc/result/workdir/result.pkg.kio` | `import result/result {type Result; fn ok; fn err}` |
| `vec` | `test-data/poc/vec/workdir/vec.pkg.kio` | `import vec/vec {type Vec; fn empty; fn push_back}` |

Run `kio dep fetch` from the consumer package root after adding each
dependency declaration. This writes its lock and materialized source under the
chosen local name; import those modules as shown. A package's public host
requirements still need the consumer's ordinary bridge/rehosting setup, as
explained in [Package files and bridges](pkg.md).

For the full public interfaces, supply the following host capabilities in
their re-rooted modules, or [rehost](pkg.md#rehosting-a-dependencys-host-items-with-rehost)
them to compatible consumer declarations. Include the chosen providers in
your consumer's `bridge` block.

| Module | Host types and operations |
| --- | --- |
| `dict/dict` | `I32` (`role(i32)`); `add: (I32 & I32) -> I32`; `loop` |
| `queue/queue` | `I32` (`role(i32)`); `add_i32: (I32 & I32) -> I32`; `loop` |
| `option/option` | `String` (`role(str)`); `string_concat: (String & String) -> String`; `loop` |
| `vec/vec` | `I32` (`role(i32)`); `add`, `sub`, `mul`, `div`, `mod: (I32 & I32) -> I32`; `eq_i32`, `leq_i32`, `lt_i32: (I32 & I32) -> Bool`; `loop` |
| `result/result` | No host declarations of its own |

The public `Bool` names in `list/list`, `option/option` and `vec/vec` are
transparent aliases of each library's imported `control.Bool`. The Boolean
host declaration therefore lives under `list/macros/control`,
`option/macros/control` or `vec/macros/control`, respectively. Include that
provider in the bridge, or rehost it to an existing Boolean-capability module.

Each `loop` has the same scheme, `[S][R] ((S -> (S | R)) & S) -> R`:
call the step with the current state, continue with its left result, and return
its right result. The [list consumer below](#a-list-consumer-with-host-bindings)
shows this traversal capability and all of `list`'s bindings in a complete
package.

Library dependencies can introduce further host requirements. Inspect the
selected package's actual bridge contract and include the capability providers
needed by the interfaces you expose. Rehosting shares their exact Kio type
identities with existing consumer declarations; using the same host integer
representation alone does not unify distinct Kio types. Constructors in
`option/core`, `queue/core` and `list/core` use no host operations; importing
those alone does not require traversal functions' host capabilities.

### A list consumer with host bindings

The `list/core` constructors above use no host operations. Traversal functions
in `list/list`, such as `foldl`, use the library's declared host capabilities.
For example, a consumer can total two prices represented as integer cents.
Use the `list.dep.kio` above and create `invoice.pkg.kio`:

```kio {variant=package}
pkg invoice;

bridge {
  invoice;
  list/core;
  list/list;
  list/macros/control;
  list/macros/testapi
}
```

Then create `invoice.kio`:

```kio {placeholder={"list/core":"core","list/list":"list"}}
mod invoice;

import list/core as lists;
import list/list as traversal;

pub fn subtotal(first: traversal.I32, second: traversal.I32) -> traversal.I32 {
  let prices = lists.cons(first, lists.cons(second, lists.nil[_]));
  traversal.foldl(traversal.add_i32, 0, prices)
}
```

The bridge exposes the library's host requirements under `list/list`. Supply
these bindings in the host implementation; the names below are relative to
that module:

| Binding | Type or operation |
| --- | --- |
| `I32`, `String` | Host types with roles `i32` and `str` |
| `Bool` | Alias of the host type `list/macros/control.Bool` |
| `add_i32`, `sub_i32` | `(I32 & I32) -> I32` |
| `eq_i32`, `lt_i32` | `(I32 & I32) -> Bool` |
| `loop` | `[S][R] ((S -> S \| R) & S) -> R`; continue on the left arm, return the right arm |
| `string_concat` | `(String & String) -> String` |

The `list/macros/control` bridge supplies `Bool` with `role(bool)`.
The macro dependency also requires host types under `list/macros/testapi`:
`Bool` (`bool`), `I32` and `Int` (both `i32`), and `String` (`str`).

These are the bridged modules' requirements even though this particular
function only uses addition and iteration. With those bindings, calling
`subtotal(125, 250)` returns `375`. The [host integration guides](../README.md#host-integrations)
show how to supply typed host values and functions in each language. To reuse
bindings already provided by a consumer module, put
`rehost list/list to your_module;` and
`rehost list/macros/control to your_module;` in `list.dep.kio`, then bridge
that provider;
[rehosting](pkg.md#rehosting-a-dependencys-host-items-with-rehost) explains the
compatible-name and type requirements.

Fetch and check from the directory containing `invoice.pkg.kio`:

```sh
kio dep fetch
kio check
kio test
```

<!-- markdownlint-disable-next-line MD033 -->
<a id="the-macro-library"></a>

## The macro library

[`macros`](../../test-data/poc/macros/) is the reusable compile-time structural
toolkit used throughout these guides. Its modules are deliberately separate so
a consumer can import a narrow vocabulary:

- `spine_macros` supplies the everyday `fit`, `reorder_*`, `narrow_*`,
  `widen_*`, `flatten_*`, and `one_*` product/sum adapters.
- `row_macros` supplies `record` / `record_`, `with` / `with_`, and `select`
  for ordinary product slots. The [row-typed record guide](products.md) combines
  these operations with nominal fields and open product tails.
- `tuple_macros` supplies tuple operations such as `head`, `tail`,
  `concat`, `flatten`, `group`, `zip`, and `map`.
- `match` supplies exhaustive, first-match structural dispatch through
  `match`.
- `control` supplies lazy `if` branches, `scope` blocks, and Boolean
  `and` / `or` macros with `&&` / `||` operator spellings, plus `not`.
- `sequence` supplies `Bind` and `Sequence` types and `do` blocks that
  sequence actions through an explicitly supplied bind function.
- `lookup` supplies the advanced conditional cross-sum helpers `lookup` and
  `contains`. They inspect whether an active sum branch contains a requested
  type; they operate on sum branches rather than product slots.
- `derive` composes an explicit set of candidate rules to build one target
  value through `derive`.
- `type_of` reflects a requested type into the library's user-level type
  representation.
- `show` synthesizes a string renderer for supported structural shapes.
- `algebraic_macros` supplies the DNF-level `iso`, `into`, `onto`,
  `align`, `ease`, and `atom` palette.
- `macro_util` contains shared implementation types and folds for
  macro authors rather than an application-facing surface.

For example:

```kio {ignore}
import macros/match {macro match}
import macros/control {type Bool; macro if; macro scope}
import macros/sequence {type Bind; type Sequence; macro do}
import macros/lookup {macro contains; macro lookup}
import macros/spine_macros {macro fit; macro widen_sum}
import macros/row_macros {macro record; macro record_; macro select; macro with; macro with_}
```

`control` declares the concrete `Bool` host type used by its Boolean operations.
The [operator guide](operators.md#delay-an-operand) shows the selective imports
and delayed right operands for `&&` and `||`. Use that same
imported type for conditions and Boolean-producing host functions. If your
package already declares `Bool`, add `rehost macros/control to app/types;`
to `macros.dep.kio`, where `app/types` is the provider module for that type,
and fetch again. The generated alias keeps both sides at one exact identity;
matching `role(bool)` alone does not unify distinct host types. Include the
chosen provider in the package bridge. See
[rehosting](pkg.md#rehosting-a-dependencys-host-items-with-rehost).

[Defining macros](macros.md) explains the reflection ABI behind
these modules. [Building and testing macros](macro-authoring.md)
develops complete small implementations and their callers. [The macro
library case study](../poc/macros.md) covers the library's operations, how its
modules fit together, and its laws; the linked source provides implementation
details.

## The optics library

[`optics`](../../test-data/poc/optics/) provides lenses, prisms, and
isomorphisms as pure function pairs. It includes composition, `view`, `set`,
`over`, prism preview/review, product lenses, sum prisms, and structural
isomorphisms. It depends on `macros` for the spine adapters, so consuming its
materialized package also consumes the committed dependency closure.

Import the public module through the chosen root:

```kio {ignore}
import optics/optics {type Lens; fn compose_lens; fn view; fn set}
```

See [The optics library](../poc/optics.md) for the complete executable tour and
its `equiv` laws.

## Case studies, not drop-in libraries

Two POCs teach integration patterns rather than offering a single reusable
module surface:

- [Higher-kinded types](../poc/hkt.md) assembles kinded brands, explicit
  `Functor`/`Monad` dictionaries, monadic `do`, and `derive`. Use it as a
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

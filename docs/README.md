# Kio documentation

Learn Kio here — tutorials to get started, topic guides for specific tasks, case studies of complete packages, and host guides for embedding Kio in a runtime. For the precise contracts, see [`specs/`](../specs/).

## Tutorials

New to Kio? Start here — these read top to bottom.

- [The Kio language](tutorials/language.md) — a guided tour: packages, modules, functions, types, tuples and labels, sums and pattern matching, polymorphism, elaborator calls, and structural glue.
- [Getting started with the tooling](tutorials/tooling.md) — one full lap of the dev loop on a tiny package: `kio init`, `fmt`, `check`, `test`, `build`, and sealing the contract surface with `kio sig`.
- [Filling a shared `match!` result](tutorials/elaborator-hole-filling.md) — start with an explicit target, diagnose disagreeing clauses, and then infer the same common result in either clause order.

For exact command-line behavior, see [`specs/cli.md`](../specs/cli.md).

## Guides

Once you know the basics, choose the task below. Ordinary source starts with
the language guides; compiler pseudo-modules are a reference for elaborator
authors, not the starting point for products and sums.

### Writing the language

- [Understanding typechecking](guides/typechecking.md) — where types come from, how calls share information, where bindings stop inference, and when to add annotations.
- [Structural products and row types](guides/products.md) — tuples, labels, row-polymorphic parameters, access, update, and product order.
- [Structural sums and pattern matching](guides/sums.md) — `|`, named label arms, imported `widen_sum!`, and exhaustive `match!` clauses.
- [Aliases, newtypes, visibility, and purity](guides/declarations.md) — transparent aliases, nominal and existential types, explicit recursive-data scopes, scoped exports, and transitive purity.
- [Recursion](guides/recursion.md) — `rec newtype` / type groups versus `rec(loop)`, mandatory recursive call markers, and `rec(poly)` / `rec(cont)` annotations.
- [UFCS calls](guides/ufcs.md) — `.>`, `.>>`, `.<`, and `.<<` call splices.
- [Operators](guides/operators.md) — fixed `op` declarations and the slot vocabulary.
- [Variadic operators](guides/variadic-operators.md) — four fold modes, compound elements, finalizers, and collection literals.
- [Error handling](guides/error-handling.md) — `(T | !)` sums and explicit failure flow.
- [Dependency injection](guides/dependency-injection.md) — capabilities as product values, labeled bundles, and row-shaped subsets.
- [Higher-kinded types](guides/higher-kinded-types.md) — kinds, type-constructor application, instance newtypes, and imported `do!` sequencing.

### Packages and libraries

- [Package files and bridges](guides/pkg.md) — host contracts, targets, dependencies, materialization, rehosting, and retyping.
- [Using libraries](guides/using-libraries.md) — the local/Git workflow, re-rooted imports, and the reusable repository libraries.
- [Dynamic loading](guides/dynamic-loading.md) — emitting, loading, contract-matching, instantiating, and calling a Kio' package at runtime.

### Testing, documentation, and debugging

- [Testing with `equiv`](guides/equiv.md) — symbolic partial evaluation, residual normal forms, and `kio test`.
- [Writing Kiodoc](guides/kiodoc.md) — checked Markdown snippets, `///` comments, references, and item directives.
- [Exploring a package with `kio repl`](guides/repl.md) — load, query, browse, and navigate modules interactively.
- [Debugging with Kio'](guides/debugging-with-kio-prime.md) — inspect the lowered core when surface behavior is surprising.

### Compiler-facing material

- [Defining elaborators](guides/elaborators.md) — the checked-term ABI, reflection, captures, diagnostics, and total compile-time recursion.
- [Builtin modules](guides/builtin-modules.md) — generated exhaustive reference for `__intrinsics__` and `__comptime__`.
- [The open-world story](guides/open-world.md) — what open-world compilation means for imports, inference, and elaborator design.

### Tooling

- [Shell completions for `kio`](guides/shell-completions.md) — generate and install `bash`, `zsh`, or `fish` completion scripts.
- [Installing Kio from source](guides/install-from-source.md) — build the CLI and VS Code extension and configure format-on-save.

## Case studies

End-to-end read-throughs of the runnable proof-of-concept packages under [`test-data/poc/`](../test-data/poc/). Everything a page shows is real code from the package itself — checked and run on every CI run, never simplified for the page. Read one when you want the whole picture of how a body of Kio fits together, not just the slice a topic guide isolates.

- [The optics library](poc/optics.md) — [`test-data/poc/optics/`](../test-data/poc/optics/): lenses, prisms, and isos as function pairs, the spine palette and `match!` in use, an operator DSL, and every `equiv` law.
- [Higher-kinded types](poc/hkt.md) — [`test-data/poc/hkt/`](../test-data/poc/hkt/): kinded brands, first-class instance dictionaries, `do!` pipelines over an abstract type constructor, and a `derive!` instance pick.
- [The elaborator library](poc/elab.md) — [`test-data/poc/elab/`](../test-data/poc/elab/): the source-to-target coercion palettes (algebraic and spine), `match!`, `derive!`, and the reflected-type vocabulary, documented per form with their `equiv` laws.
- [Dynamic loading](poc/dyn_load_prime.md) — [`test-data/poc/dyn_load_prime/`](../test-data/poc/dyn_load_prime/): a host loads a pre-compiled package from its emitted Kio' image at runtime, contract-matches it, and calls its exports through a universal existential surface.

## Host integrations

How to embed a Kio package in each supported host.

- [JavaScript](hosts/js.md) — building with the js backend and calling into the package from JS.
- [TypeScript](hosts/ts.md) — building with the ts backend (the JS `.js` plus a generated `.d.ts` typed skin) and calling into the package from TypeScript.
- [Python](hosts/python.md) — building with the python backend and calling into the package from Python.
- [Java](hosts/java.md) — building with the java backend and calling into the package from Java.
- [Rust](hosts/rust.md) — building with the rust backend and calling into the package from Rust.
- [Go](hosts/go.md) — building with the go backend and calling into the package from Go.
- [Swift](hosts/swift.md) — building with the swift backend and calling into the package from Swift.
- [Haskell](hosts/haskell.md) — building with the haskell backend and calling into the package from Haskell.

## Snippets

Kio code snippets in these files are written in **Kiodoc** — GitHub-flavored Markdown plus a small set of fence-attribute directives. `kio doc check` validates the snippets, `kio doc fmt --check` gates their canonical formatting in CI, and `kio doc build` renders this tree, together with each module's `///` doc-comments, into a per-module documentation site.

# Getting started with the Kio tooling

Start here when you want to run the Kio development loop on a real package. You will create a tiny `greeter` package, edit it, format it, typecheck it, test it, build it, and record its contract surface with `kio sig`.

Every command below is meant to be run from the package root as you read. The tutorial assumes only the language used in the examples; for a fuller language tour, read [the language tutorial](language.md). For exact command-line behavior, see [`specs/cli.md`](../../specs/cli.md).

The whole loop uses one program, `kio`, with a subcommand per step.

<!-- Accumulating harness: the tutorial builds one `main` module up
across Chapters 2 and 5. The host declarations live here; each {@loop}
member contributes the surface code the prose introduces. -->
<!--kio {harness=loop placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  main;
}

module main;

host type String role(str);
host fn print(p0: String) -> .;
host fn string_concat(p0: String, p1: String) -> String;

__INSERT_CODE_HERE__
-->

## Chapter 1 — Scaffold a package with `kio init`

A Kio program is a **package**: a directory holding a *package file* (the package's contract with its host, plus its build settings) and one or more *modules* (the code). `kio init` writes the smallest working package for you.

```sh
mkdir greeter
cd greeter
kio init
```

With no argument, `kio init` names the package after the current directory; pass a name to override it (`kio init greeter`). The package name must be a lowercase identifier containing at least one ASCII letter: `_a` and `_1a` are valid, but `_` and `_1` are not. Invalid names are rejected before any files are created. The command writes two files at the package root and prints what it created:

```text
created Kio package `greeter`
  greeter.pkg.kio
  main.kio
```

So the package starts as:

```text
greeter/
├── greeter.pkg.kio
└── main.kio
```

`kio init` refuses to overwrite an existing package file, so it is safe to run in an empty directory and loud in one that already looks like a package.

### What the package file declares

`greeter.pkg.kio` is the package file. Its filename stem (`greeter`) is the package name.

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/"
  }
}

bridge {
  main
}
```

Two blocks do the work:

- `build { ... }` names a compilation target (`js`, writing under `out/js/`) and a cache directory. This is what `kio build` reads.
- `bridge { ... }` lists the modules whose public surface forms the package's **contract** with its host. Here it admits `main`, the one module we have.

### The starting module

`main.kio` is the package's **root module** — just a `module <name>;` file at the package root. The generated one declares a string type and a print capability it asks the host to supply, then exports a `main` that prints a greeting:

```kio {variant=module}
module main;

host type String role(str);

host fn print(p0: String) -> .;

pub fn main() -> . { print("hello from Kio\n"(String)) }
```

A `host type` / `host fn` is a capability the **host** provides: the package declares the shape it needs and the host supplies the implementation when it loads the package. `pub` marks `main` as part of the package's exported surface. That is all the language you need to follow the rest of this tutorial.

## Chapter 2 — Write a little code

Let's add one function and call it from `main`: a `greeting` function that builds a string, with `main` printing its result.

First, the new function — deliberately typed on one cramped line so we have something for the formatter to fix in the next step:

```kio {ignore}
fn greeting(name: String) -> String {string_concat("hello, "(String),name)}
```

`greeting` concatenates a literal prefix with its argument. That call needs one more host capability — string concatenation — so add its declaration alongside the existing `host` lines in `main.kio`:

```kio {ignore}
host fn string_concat(p0: String, p1: String) -> String;
```

Finally, rewrite `main` to call `greeting`:

```kio {@loop}
fn greeting(name: String) -> String { string_concat("hello, "(String), name) }

pub fn main() -> . { print(greeting("Kio"(String))) }
```

The module now reads, top to bottom: a `module main;` header, three `host` declarations (`String`, `print`, `string_concat`), then `greeting` and `main`.

## Chapter 3 — Format with `kio fmt`

Kio has exactly one canonical style and no knobs to configure it. `kio fmt` rewrites your source to that style in place:

```sh
kio fmt
```

It prints the path of every file it changed; silence means everything was already canonical. Because we wrote `greeting` on one cramped line, `kio fmt` reports:

```text
main.kio
```

Open the file again and the body has been spaced to the house style (the `greeting` line above already shows the canonical form). Writes are atomic, so an interrupted `kio fmt` never leaves a half-written file.

In CI you want the check-only mode, which writes nothing and fails if any file is off-style:

```sh
kio fmt --check
```

`--check` exits `60` when at least one file would change (and lists those paths), `0` when everything is already canonical. Wire that into your pipeline so unformatted code can't merge.

## Chapter 4 — Typecheck with `kio check`

`kio check` runs the whole front end — parse, resolve, typecheck — and reports any errors. It writes no output files and touches no network; it is the fast command to run after every edit.

```sh
kio check
```

A clean run is **silent and exits `0`**. If you have a type error, `kio check` prints it and exits with a code identifying the *category* of error (parse, type, name resolution, …) so scripts can branch on the kind of failure without scraping messages; the table is in [`specs/exit-codes.md`](../../specs/exit-codes.md).

Try breaking it on purpose: pass `greeting` an integer instead of a string and re-run `kio check`. You will get a type error pointing at the offending call. Fix it back, re-run, and you get silence again.

Carets mark the error in a source excerpt; secondary underlines show related
context such as a type annotation. Tabs expand for display so these marks stay
aligned. The header's column still counts characters in the original source,
including each tab as one character.

For an imported function, the related annotation can be in another module.
The diagnostic names that file even when you call through a local alias;
the function's signature explains the requirement, not the type's definition.

The module header is one path, such as `module main;`, not a package name
followed by a module name. Package declarations belong in the separate
`*.pkg.kio` file. Diagnostics explain this distinction when a header contains
adjacent names.

Types and values also have different punctuation: `()` is the unit value,
while `.` is its type; `(a, b)` is a tuple value, while `A & B` is a product
type. In an editor using `kio lsp`, the corresponding type-position
diagnostics offer quick fixes that preserve comments, nested type arguments,
and component grouping. Applying a fix rechecks the edited document.

Invalid-character diagnostics identify the complete source character.
Unicode text is valid in strings and comments, while identifier spellings use ASCII.

## Chapter 5 — Test with `kio test`

Kio's built-in testing primitive is the `equiv` declaration: a claim that two or more expressions reduce to the same value. `kio test` runs the same front end as `kio check`, then evaluates every `equiv` in the package and reports which ones hold.

Add a claim to `main.kio` that pins down `greeting`'s behavior:

```kio {@loop}
equiv greeting_kio() {
  greeting("Kio"(String));
  string_concat("hello, "(String), "Kio"(String))
}
```

The claim says "calling `greeting` with `"Kio"` is the same as concatenating the prefix with `"Kio"` directly." Run the tests:

```sh
kio test
```

Each `equiv` prints a `pass` or `fail` line, and the run ends with a summary:

```text
  pass equiv `greeting_kio` in greeter/main

result: 1/1 equiv block passed
```

A package with no `equiv` blocks prints `no equiv blocks found` and exits `0` — having no tests is not a failure. A genuine equivalence failure exits `50`. For more on writing claims, see [Testing with `equiv`](../guides/equiv.md).

## Chapter 6 — Build with `kio build`

`kio build` reads the `build { ... }` block from the package file, hands each declared target to its backend, and writes output under that target's `out` directory. It typechecks first, so a package that fails `kio check` never reaches codegen.

```sh
kio build          # build every target in the build block
kio build js       # build only the target whose id is "js"
```

With no argument every target is emitted; naming target ids builds only those (an unknown id is an error). Our package declares one `js` target, so both commands here do the same thing. The emitted JavaScript lands under `out/js/`. What the artifact looks like, and how to call into it, is backend-specific — see the host guide for your target: [JavaScript](../hosts/js.md), [TypeScript](../hosts/ts.md), [Python](../hosts/python.md), [Java](../hosts/java.md), [Rust](../hosts/rust.md), [Go](../hosts/go.md), [Swift](../hosts/swift.md), or [Haskell](../hosts/haskell.md).

## Chapter 7 — Seal the contract surface with `kio sig`

The package's **contract surface** is everything its `bridge` block exposes: the host capabilities it requires and the `pub` items it offers back. When that surface changes, consumers that loaded the package care. `kio sig` records each version of the surface in a changelog file (`greeter.sig.kio`) and gates changes on backward compatibility. The full contract is in [`specs/versioning.md`](../../specs/versioning.md); here we walk the everyday path.

Run it bare first. With no changelog yet, `kio sig` is non-mutating and points you at the first step:

```sh
kio sig
```

```text
kio sig: `greeter` has no compatibility changelog yet — `kio sig commit` seals the first version
```

### Stage, then commit

A version is a mutable **draft** until you seal it. `kio sig stage` records the current surface as a compatible delta into that draft; `kio sig commit` seals it and bumps the version number.

```sh
kio sig stage
kio sig commit -m "Initial contract: String, print, string_concat, main."
```

`stage` writes the draft (the host type `String`, the host fns `print` and `string_concat`, and the export `main`). `commit` seals it as `v(1)`; the `-m` message is stored with the version and shown by the log. A bare `kio sig stage` only ever records a *compatible* change — if your edit **broke** the last sealed surface, it errors until you acknowledge the break with `kio sig stage --force`.

### Review the history

In that history, `add` introduces a declaration, `modify` changes one that is
still present, and `remove` drops one. Bringing back a removed declaration is
another `add`, not a `modify`, even though its earlier declaration stays in the
history.

`kio sig log` pretty-prints the changelog, each version with its commit message:

```sh
kio sig log
```

It takes read-only display filters: `kio sig log --breaking` shows only versions that carry a breaking change, and `kio sig log --since 1` scopes the output to versions after `v(1)`. (When the history grows unwieldy, `kio sig compact <version>` collapses the additive history before a version into a single boundary block — a detail for later.)

### The CI gate

`kio sig status` is the command CI runs to enforce the changelog. It compares the live surface against the last sealed version and exits:

```sh
kio sig status
```

- `0` — the changelog is up to date with the source;
- `81` — there is an unrecorded but **compatible** drift (run `kio sig stage`);
- `82` — a break is recorded but not yet sealed (run `kio sig commit`);
- `80` — the source **breaks** the sealed contract and the break is unrecorded (acknowledge with `kio sig stage --force`, or reconcile the source).

Right after our `stage` / `commit`, `kio sig status` is clean and exits `0`. Add a new `pub fn` later and it reports `81`; `kio sig stage` then records it and clears the gate again. Bare `kio sig` prints the same status summary without changing anything, so it is the safe command to run when you just want to look.

## Chapter 8 — The loop, end to end

An editor connected to `kio lsp` can rename a declaration and its references
together. Renaming a newtype updates its uses in signatures, aliases, payloads,
annotations and explicit call type arguments. A same-named type parameter is
a separate binding and stays unchanged.

Renaming an explicit newtype constructor to its projector's name (or
the reverse) is refused: those two members share one newtype's namespace.
Members of a different newtype do not reserve names for ordinary functions or
local variables.

That is one full lap. Day to day you'll spend most of your time in the first two steps:

1. Edit a module.
2. `kio check` — catch type errors fast.
3. `kio fmt` — canonicalize before committing (`kio fmt --check` in CI).
4. `kio test` — confirm the `equiv` claims still hold.
5. `kio build` — emit the artifact when you're ready to run it in a host.
6. `kio sig stage` → `kio sig commit` when the contract surface changes; `kio sig status` as the CI gate.

## Chapter 9 — Where to go next

- [The Kio language](language.md) — a guided tour of the language itself: types, functions, sums, polymorphism, and structural glue.
- [`specs/cli.md`](../../specs/cli.md) — the exact command-line contract.
- [Exploring a package with `kio repl`](../guides/repl.md) — load a package interactively and query its types, docs, and source.
- [Host guides](../hosts/) — run a built package from JavaScript, TypeScript, Python, Java, Rust, Go, Swift, or Haskell.

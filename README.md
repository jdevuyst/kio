# Kio

<!-- markdownlint-disable-next-line MD033 -->
<div align="center">

[![Kio logo](website/branding/kio-logo-readme.png)](https://jdevuyst.github.io/kio/)

</div>

**Kio is an ultra-portable, embeddable programming language based on polymorphic lambda calculus with higher-kinded types.**

Website: **[jdevuyst.github.io/kio](https://jdevuyst.github.io/kio/)**

Current version: 0.1.0 (pre-1.0; expect breaking changes.)

## In a Nutshell

Kio transpiles to other languages, inheriting their reach — write a package once and run it in any of [eight host languages](docs/hosts/).

The language is designed to be:

- **Hosted** — Kio packages are components that run inside a host language/runtime, not standalone programs. The host supplies the package's capabilities (numerics, strings, I/O, …) and decides which exposed package entries to invoke.
- **Dynamically loadable** — alongside compile-time linking, hosts can load pre-compiled Kio programs at runtime. The interface stays statically typed, so type errors are caught at compile time.
- **Developer-friendly** — great diagnostics, fast feedback loops, and first-class tooling are part of the design contract, not afterthoughts.

## Hello World

```kio
module hello;

host type String role(str);
host fn print(str: String) -> .;

pub fn main() -> . { print("hello from Kio\n") }
```

The `host` declarations name the capabilities the package needs from its host — here a `String` type and a `print` function — while `main` is the entry the host chooses to call.

## Install

macOS, Linux, and Windows — install the `kio` command with one line:

```sh
curl -fsSL https://jdevuyst.github.io/kio/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://jdevuyst.github.io/kio/install.ps1 | iex
```

Prefer to build it yourself? `cargo install kio-lang --bin kio`. Prebuilt archives and checksums for every platform are on the [releases page](https://github.com/jdevuyst/kio/releases).

## Language Features

- **User-defined elaborators (`elab`)** — imported compile-time metaprograms generate typed code. For example, `reorder_prod!((a, b), B & A)` reorders a product to match the requested type. Familiar control forms such as `if! condition { yes() } else { no() }` are library-defined too, rather than built-in control syntax.
- **Sound, decidable type system** — type inference and checking always terminate.
- **Open-world compilation** — adding new declarations to a module body never breaks existing code that compiled before.
- **Predictable polymorphism with opt-in relief** — bidirectional elaboration over explicit binders (rank-N permitted), no implicit generalization, no backward type flow from later uses. Relief comes in two forms: inference fills uniquely-determined type-arg slots, and elaborator metaprograms fill in the value-level glue where writing it by hand would be tedious.
- **Structural products, sums, and labels** — types compose via `&` and `|` directly; user-defined names enter through generated nominal label types, not through named constructors or fields. Pattern matching, recursive types (grounded at `newtype` boundaries), and row-typed records and variants all emerge from combining `&`, `|`, and `labels` (plus a type variable for the open tail) — no typeclasses.
- **Higher-kinded types** — kind-annotated binders (`[*F]` is kind `*→*`) and direct type application (`F(A)`), so generic code can quantify over a type-constructor — a Church-style fragment of System F-ω. Higher-kindedness enables generic code such as library-defined `do! bind { … }` sequencing, where `bind` is a function you supply.
- **UFCS calls** — any function can be called receiver-first: `r.>f(x)` is exactly `f(r, x)`, with four dot-splice variants (`.>`, `.>>`, `.<`, `.<<`) that place the receiver in different argument slots. Blockless elaborator calls support UFCS too; block calls use their direct `name! … { … }` form.
- **User-defined operators** — Kio has no built-in operators, so they are all yours to define. An `op` binds a pattern of symbol tokens and operand slots to a function, and can be prefix, infix, postfix, or n-ary — `+`, right-associating `::`, a ternary `? :`, and so on. `varop` adds a bracketed, variadic form that folds elements through a step function, handy for lists, dicts, and other collections.
- **Strongly normalizing core** — every well-typed program in Kio', the desugared core, terminates (excluding host execution).
- **User-visible elaboration** — every surface program reduces to Kio', and `:normalize` in the REPL prints the residual normal form, so you can see exactly what a program became.
- **Provable equivalence (`equiv`)** — an `equiv` block asserts that two or more expressions reduce to the same normal form under shared type and value binders; `kio test` discharges it by normalizing in Kio'. The check is decidable and exhaustive — nothing runs, nothing is sampled — so algebraic laws, refactors, and optimizations are proven at build time.

## Tooling

- **`kio` CLI** — typecheck, build, test, format, generate docs, manage caches.
- **`kio sig`** — versioned contract-surface changelog: records a package's host/export interface, classifies a change as compatible or breaking before it ships, and gates CI.
- **Language server** — LSP implementation with hover, goto, references, completion, symbols, formatting, semantic tokens, rename.
- **Editor support** — tree-sitter grammar, VS Code extension, TextMate grammar.
- **REPL** — module inspector with syntax highlighting, completions, inline hints, fuzzy matching, multi-line input, and introspection commands.
- **Kiodoc** — doc generator with validated embedded Kio snippets.

## Documentation

- [`docs/`](docs/) — pedagogical material: tutorials, guides, host integrations.
- [`specs/`](specs/) — authoritative contracts.
- [`DESIGN.md`](DESIGN.md) — language goals and design rationale.
- [`test-data/poc/`](test-data/poc/) — reference modules you can drop into your own package.
- [`ROADMAP.md`](ROADMAP.md) — in-flight design threads.

## Development Setup

The easiest way to get a working build environment is the dev container at [`.devcontainer/`](.devcontainer/), either locally with VS Code's "Reopen in Container" or in [GitHub Codespaces](https://github.com/features/codespaces). Local setup without the container is documented in [INSTALL.md](INSTALL.md).

## Contributing

Kio is maintainer-driven and built almost entirely with AI: the compiler, specs, docs, and tooling are AI-written, directed and reviewed by the maintainers, and aren't open to pull requests. The most useful contribution is a precise bug report — ideally a minimal `.kio` reproducer. One kind of pull request is welcome, though: an example program or library added under [`test-data/contrib/`](test-data/contrib/), which then joins CI and guards against regressions. Commits need DCO sign-off (`git commit -s`). See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([`LICENSE-MIT`](LICENSE-MIT) or <https://opensource.org/license/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

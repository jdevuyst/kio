# tools/tree-sitter-kio

Tree-sitter grammar for Kio. One of the three grammar-derived highlighters described in [`specs/language.md`](../../specs/language.md) — companion to `kio debug tokens` (the reference tokenization, in `kio-rs/src/tokens.rs`) and the TextMate grammar in [`../textmate-kio/`](../textmate-kio/).

## What this grammar does

Lexical-layer tokenization of Kio source plus a permissive structural skeleton for the forms that affect highlighting: Kio-family file headers, package blocks, module-first imports, declaration heads, signatures, label entries, calls, UFCS, and simple block forms. The token-kind vocabulary is the [`TokenKind`](../../kio-rs/src/tokens.rs) enum (the source of truth, written out by `kio debug tokens`); the agreement check at [`ci/checks/orchestrators/highlight-agreement.sh`](../../ci/checks/orchestrators/highlight-agreement.sh) verifies — over the curated fixtures at [`test-data/highlight-corpus/`](../../test-data/highlight-corpus/) — that every positive tree-sitter classification matches `kio debug tokens`. Bracket-bearing runs and bracket-led call arguments whose roles require expression detail beyond this skeleton are left unhighlighted under an exact aggregate source span.

Imports carry their own complete operator grammar; highlighting never reads a
provider to recover it. Selective lists retain fixed-pattern groups and
variadic separators. Fixed and variadic declarations classify their grammar
tags, and the four variadic modes plus optional finalizer classify their clause
heads; the same words remain ordinary identifiers outside those positions.

Highlight queries live in [`queries/highlights.scm`](queries/highlights.scm); editor integrations (Neovim, Helix, Zed, …) consume them directly.

The grammar still keeps a token-soup fallback for arbitrary fragments and unsupported expression detail. `source_file` is a repeat of structural units or fallback tokens, so complete files get parser-context highlighting where the skeleton knows the shape, while incomplete snippets still produce useful leaf tokens.

## Generating the parser

`tree-sitter generate` produces `src/parser.c` (committed alongside `grammar.js`). Consumers compile against the committed parser, so they don't need to run `tree-sitter generate` themselves.

```sh
cd tools/tree-sitter-kio
tree-sitter generate
tree-sitter test -p .
```

## Tests

Tree-sitter's parser-correctness corpus lives at `test/corpus/` (the path tree-sitter's `test` subcommand walks). `tree-sitter test -p .` runs every case and diffs the produced parse tree against the recorded one.

The lexical-agreement corpus at [`test-data/highlight-corpus/`](../../test-data/highlight-corpus/) is a separate concern — those fixtures verify cross-tokenizer agreement (tree-sitter ↔ `kio debug tokens` ↔ TextMate) and are run by [`ci/checks/orchestrators/highlight-agreement.sh`](../../ci/checks/orchestrators/highlight-agreement.sh).

## Distribution

Distribution targets the npm package `tree-sitter-kio` plus GitHub releases with WASM + platform-specific natives. Generated `src/parser.c` is committed so the publishing pipeline can build native + WASM without regenerating.

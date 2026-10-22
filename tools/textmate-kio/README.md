# tools/textmate-kio

TextMate grammar for Kio, in [`kio.tmLanguage.json`](kio.tmLanguage.json). One of the three grammar-derived highlighters described in [`specs/language.md`](../../specs/language.md) — companion to `kio debug tokens` (the reference tokenization, in `kio-rs/src/tokens.rs`) and the tree-sitter grammar in [`../tree-sitter-kio/`](../tree-sitter-kio/).

## What this grammar does

Regex-only token classification — keywords, literals, comment delimiters, identifier / operator shapes. Suitable for VS Code's static highlighting (loaded before any language server attaches) and any other editor that consumes TextMate grammars, plus GitHub Linguist for ` ```kio ` code blocks.

The grammar maps Kio's tokens to TextMate scope conventions; the canonical token-kind vocabulary is the [`TokenKind`](../../kio-rs/src/tokens.rs) enum (the source of truth, written out by `kio debug tokens`). The agreement check at [`ci/checks/orchestrators/highlight-agreement.sh`](../../ci/checks/orchestrators/highlight-agreement.sh) verifies — over the curated fixtures at [`test-data/highlight-corpus/`](../../test-data/highlight-corpus/) — that TextMate's positive classifications match `kio debug tokens`. TextMate may be **silent** on tokens it can't classify (regex limit); only positive classifications must agree.

## Limitations

Ordinary symbol words receive operator scopes; dots and square brackets keep
their native lexical roles. Operator association and slot validation belong to
the compiler. Neither this grammar nor standalone Tree-sitter reconstructs a
consumer's dynamic operator grammar.

TextMate uses bounded `begin` / `end` states for strings, module-first import
paths and selection groups, row lets, and variadic callable clauses. An import's
written grammar is sufficient; no provider is read. Token quotation and
variadic separator semicolons stay inside the parenthesized selection list.
Grammar-owned `op`/`varop` tags and four-mode/finalizer clause heads receive
keyword scopes; ordinary contextual names keep identifier or entity scopes.
Positive declaration-head scopes require bounded same-line syntax evidence.
An incomplete or line-split head and its unresolved contextual words stay
unscoped; neutral recovery ends at the construct's boundary or a following
recognizable declaration. Canonically spaced marked heads retain callable-clause
scopes. Compact heads beyond the regex engine's authentication capacity retain
lexical coloring while callable-clause words stay neutral. The tree-sitter
grammar supplies deeper structural context.
Inside square value regions, a name-plus-bang at a possible closing edge stays
neutral: regex context alone does not distinguish a closing `!` marker from a
native elaborator suffix. Interior calls such as `helper!(arg)` retain their
elaborator scopes.

## Distribution

Distribution targets Marketplace / Open VSX / GitHub Linguist registration. The grammar itself is zero-build — `kio.tmLanguage.json` ships as-is.

# Highlight corpus

Hand-written fixtures exercising the canonical token-kind vocabulary
(defined by `TokenKind` in `kio-rs/src/tokens.rs`). Each
fixture is run through `kio debug tokens` and the resulting
JSON is locked in `expected.tokens.json`.

The driver is `ci/checks/orchestrators/highlight-tokens.sh`. Pass `-u` to refresh
every fixture's `expected.tokens.json` after intentional output
changes (e.g. the canonical vocabulary grew a kind).

## Fixture layout

```text
NN_<topic>/
  source.kio              # the input
  expected.tokens.json    # the locked `kio debug tokens source.kio` output
```

`source.kio` may be any byte sequence that lexes successfully —
fixtures are not required to typecheck or to parse against the full
grammar; the highlighter is a pure tokenization layer.

## Coverage

The fixtures sweep every kind in the canonical vocabulary, with at
least one fixture per kind:

- `01_minimal` — single ident, smoke check
- `02_keywords` — keyword-shaped words in isolation remain ordinary identifiers,
  including `if`, `else`, `match`, and `do`
- `03_elaborator_bang_calls` — algebraic (`iso!` / `into!` / `onto!` /
  `align!` / `ease!` / `atom!`), spine-palette (`reorder_sum!` /
  `reorder_prod!` / `narrow_sum!` / `narrow_prod!` / `widen_sum!` /
  `widen_prod!` / `flatten_sum!` / `flatten_prod!` / `one_sum!` /
  `one_prod!` / `fit!`), and dispatch (`match!`) elaborator-bang forms,
  plus an arbitrary user elaborator and a non-fusion intervening-space case
- `04_operators_and_brackets` — reserved (`builtin`) ops vs. user ops,
  brackets vs. separators
- `05_literals` — strings, ints, floats, bools
- `06_comments` — line comments at file start, between tokens, and
  same-line trailing
- `26_doc_comments` — `///` doc-comment lines (`comment.doc`) and
  `////` rulers (still `comment.line`)
- `28_label_value_labels` — label names in `{label = value}` construction
  classify the same way as label declaration entries
- `29_file_variants` — signature changelog, dependency declaration, and
  dependency lock file syntax; package files are covered by the package
  keyword and module smoke fixtures
- `30_import_label_item` — a comment/newline-bearing selective label item and
  adjacent expression shorthand are label references, while whitespace-
  bearing singleton blocks stay identifiers and contextual `import(...)` stays
  a function call
- `27_import_operator_grammar` — full fixed and variadic selected grammars
  retain their tags, slots, separators and delimiters
- `32_callable_declaration_targets` — fixed, variadic and elaborator
  callable targets retain ordinary function and type-member roles across trivia
- `33_contextual_variadic` — `variadic` and mode-shaped ordinary names
  stay identifiers outside actual declarations, including calls and comments
- `34_incomplete_variadic_recovery`, `37_incomplete_operator_recovery`
  and `38_incomplete_import_recovery` — incomplete declaration/import heads
  stay neutral and do not capture the following identifier or function
- `39_variadic_modes_and_imports` — all four primary modes, finalizers,
  complete same-line and multiline heads, and contextual callable names
- `31_contextual_pure_calls` — the function-only `pure` modifier remains a
  contextual identifier in unqualified and qualified call position
- `35_qualified_import_same_leaf_alias` — a qualified import alias that repeats
  the provider path's final segment classifies both path and alias occurrences
  as module names, while the intervening contextual `as` is a declaration
  keyword
- `36_contextual_keyword_roles` — `as` classifies as a declaration keyword
  in multiline row-let syntax with leading, repeated interior, and trailing
  commas and comment trivia, while duplicate-spelled labels, binders,
  references, import path leaves, and aliases retain their ordinary roles
- `07_placeholders_and_slots` — `_` / `__` / `___` slot tokens, `#` /
  `#1` / `#42` placeholders, and `.#` / `.$$$$` placeholder-lambda
  intros
- `08_module_smoke` — a tiny end-to-end module mixing several kinds

When the canonical vocabulary grows, add a fixture exercising the new
kind here in the same change.

## Relationship to `test-data/goldens/`

This corpus is independent of `test-data/goldens/`. Goldens drive the
end-to-end compile pipeline (parse → typecheck → build → run); this
corpus drives the lexer-level tokenization only.

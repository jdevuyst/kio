# Kiodoc

This document specifies the **Kiodoc** directive contract — the small set
of fence-attribute directives that let Kio source blocks inside
GitHub-flavored Markdown files be validated by the `kio doc` subcommand
and other consumers of the same contract.

For the CLI surface of `kio doc`, see [`cli.md`](cli.md) § `kio doc`.

## Doc-comment input surface

`kio doc` also reads `///` doc-comments from `.kio` source files.
For each top-level declaration (and the module node itself), it
treats the doc-comment body as a Kiodoc-bearing Markdown fragment
and runs the same fence-attribute machinery used for `.md` files.

### Fence modes inside `///` doc-comments

Only two modes are supported:

- **`{@}`** — the surrounding module is the harness (see below).
- **`{ignore}`** — skip the snippet. Same semantics as in `.md`.

The following modes are **not** supported inside `///`
doc-comments and are reported as runner errors:

- **`{@NAME}`** — rejected with: "inside a `///` doc-comment, the
  harness is the surrounding module — use `{@}` instead of
  `{@NAME}`."
- **`{}`** standalone — rejected with: "inside a `///`
  doc-comment, use `{@}` to wrap the snippet in the surrounding
  module — `{}` standalone is not supported in doc-comments (use
  a `.md` tutorial for standalone snippets)."
- **`harness=NAME`** — rejected; a custom harness template is not
  applicable inside a doc-comment.
- **An attribute-less ` ```kio ` fence** — rejected with:
  "kio fence in doc-comment lacks an attribute list: write `{@}` to
  use the surrounding module as harness, or `{ignore}` to skip."

A doc-comment fence's attribute vocabulary is therefore `{@}`,
`{ignore}`, and `check_exit_code=N`. `check_exit_code=N` composes
with `{@}` the same way it composes with `{@NAME}` in `.md` files:
`{@ check_exit_code=14}`, the `@` opening the list as a harness
reference always does.

The run-trigger attributes (`stdout`, `stderr`, `run_exit_code=N`)
pair a snippet with a following output fence, which is a `.md`
document form; a doc-comment fence carrying one is a runner error.
`variant=KIND`, `file`, and `placeholder=` are runner errors here
too: a doc-comment snippet is a module body in the `.kio` file that
carries it, so there is no other file kind to declare, no
file-backed fence to mark, and no harness template to substitute
into.

### The `{@}` form

The snippet body is wrapped in a **synthetic top-level fn** and
appended to the surrounding module source:

```
fn kiodoc_example_<MODULE>_<DECL>_<NN>() -> . {
    <snippet body>
}
```

where:

- `<MODULE>` and `<DECL>` are stable names derived from the
  module path and the documented item's name (`<DECL>` is omitted
  for module-level doc-comments).
- `<NN>` numbers multiple snippets on the same item from `0`.

The synthetic fn is appended to the module source for the
purposes of typechecking only — `kio build` and other commands
never see it.

Inside the synthetic fn body, all items in the surrounding module
(including private ones) and the module's `import` block are in
scope. The documented item is visible by its declared name.

**The snippet body is a block.** It may contain let-bindings and
expression-statements followed by a trailing expression, exactly
what a fn body accepts. It may **not** introduce top-level forms:
no `module` re-declaration, no `import` lines, no inner `fn` /
`type` / `literal` / `labels` / `op` declarations.

### Diagnostics

When `kio check` rejects a `{@}` snippet, the diagnostic locates
the error in the originating `.kio` file at the line of the
snippet's opening fence. On failure, the runner also prints the
`kio check` diagnostic that rejected the snippet, and the
assembled source (the original module text with the synthetic fn
appended) so the author can see what was actually typechecked.

A snippet whose exit code matches its declared `check_exit_code`
has *validated*, and the runner reports nothing for it — including
when the declared code is non-zero and `kio check` therefore
rejected the snippet. The rejection is the asserted outcome, so its
diagnostic is not an error to report. The same holds for a `.md`
snippet.

## Intra-doc references

Inside any Kiodoc prose — the body of a `///` doc-comment or the
non-fence text in a `.md` file — an **intra-doc reference** has the
form `` [`name`] ``: backtick-quoted name inside square brackets. The
backticks distinguish it from plain Markdown link text (`[name]` without
backticks is not an intra-doc reference and is not validated). `kio doc
check` validates each intra-doc reference and reports a Kiodoc contract
error (exit code 70, `DocError`) for any reference that cannot be
resolved.

### Syntax

```
intra-ref ::= "[`" name "`]"
name      ::= (any character except "`", "]", newline)+
```

The `name` payload is scanned verbatim. Plain Markdown inline links
`` [`name`](url) `` and reference-link uses `` [`name`][key] `` are not
intra-doc references — the scanner treats these as standard Markdown
link syntax and skips them.

### Two resolution forms

**Simple name** — a single identifier, operator token, label entry name,
or imported alias. The name is looked up in the applicable scope (see
below). Examples: `` [`print`] ``, `` [`Some`] ``, `` [`+`] ``,
`` [`m`] `` (where `m` is an `import … as m;` alias).

**Qualified path** — a module-qualified item, the module segments
`/`-separated and the item reached with `.` (`pkg/mod.name`), or an
alias-rooted member access (`m.name`). Accepted optimistically for
external source paths and for paths rooted in a known qualified-import
alias or module segment. Example: `` [`pkg/util.something`] ``.

A nonminting label-forwarding declaration has the braced identity `{field}`,
distinct from an ordinary value named `field`. It can be selected locally,
through a braced selective import, or with a qualified spelling such as
`alias.{field}` or `pkg/mod.{field}`. This declaration selector introduces no
uppercase nominal identity and does not change label construction syntax.

### Resolution context

The scope consulted depends on where the reference appears.

**Inside a `///` doc-comment** (`*.kio` source file): the surrounding
module's scope — all top-level declarations in the module (including
private items), all names brought in by `import` statements (selective
imports and qualified aliases), label entries from `labels` blocks, newtype
constructor and projector names, and operator token sequences from `op`
declarations. Intrinsic names (`__left__`, etc.) are in scope only
when `import __intrinsics__;` appears in the module.

Qualified paths whose prefix is a known `import … as m;` alias resolve
against that alias. Qualified paths whose prefix is not a known alias
are accepted optimistically (external source links cannot be validated
from the local module scope alone).

**Inside a `.md` file**: the package boundary declared in the nearest
enclosing `<name>.pkg.kio` file — the `pub` surface (host items and
exports alike) of the modules the package's `bridge { … }` block
selects. Qualified paths not rooted in a known package-boundary name
are accepted optimistically, as external references.

Only unscoped `pub` declarations and host declarations contribute boundary
names; private and scoped-public declarations and imported names do not.
A newtype's constructor or projector contributes its name only when both the
newtype and that role are unscoped `pub`. A public labels declaration contributes
its locally declared label names and generated nominal names, not reused labels.
A public nonminting forward contributes only its own braced label identity.

### Override semantics

A standard Markdown reference-link definition with the same key as an
intra-doc reference suppresses auto-resolution for that key. Authors who
want to point at an external URL can add `[name]: url` anywhere in the
document; the intra-doc scanner sees the definition and skips validation
for that name. The override key is matched verbatim (case-sensitive,
without the backticks).

Example:

```markdown
See [`print`] for details.

[print]: https://docs.example.com/print  <!-- overrides auto-resolution -->
```

### Diagnostic format

An unresolved intra-doc reference is a Kiodoc contract error reported on
the line of the broken reference:

```
<file>:<line>:<col>: unresolved intra-doc reference: `<name>` is not in scope
```

When a close name exists in the active scope (edit distance ≤ 2), the
diagnostic appends:

```
; did you mean `<suggestion>`?
```

### Open-world safety

Intra-doc reference resolution is structurally deterministic: a name
resolves if and only if it appears in the scope at the time `kio doc
check` runs. Adding new declarations to a module body enables previously
unresolvable references but never changes the outcome for references that
already resolved or already failed. This preserves the
open-world monotonicity guarantee described in
[`language.md` § Open-world design](language.md#open-world-design).

## Code-embedding directives

Three directives embed live code into Kiodoc prose. They form the
**named-item query trio** — three fixed views of one named item; the
REPL exposes the same three as the `:signature` / `:source` / `:type`
meta-commands ([`cli.md`](cli.md) § `kio repl`), under one keyword
vocabulary, one argument syntax, and one renderer per view shared
across the two surfaces. The surfaces differ only by sigil: `@` in
Kiodoc prose, `:` at the REPL prompt.

- `` [`@signature term`] `` — **inline.** When rendered, inlines the
  resolved item's pretty-printed declaration header at the directive's
  position in the surrounding paragraph. The rendered span is wrapped in
  inline code style (`` `…` `` for Markdown, `<code>…</code>` for HTML).
- `` [`@type term`] `` — **inline.** When rendered, inlines the *type
  of the resolved item's bound value* — for a `fn`, the function type,
  with no `fn` keyword, item name, or value-binder names. Wrapped in
  inline code style, like `@signature`.
- `` [`@source term`] `` — **block-promoting.** When rendered, embeds
  the resolved item's full source on its own line as a fenced code block
  with language tag `kio`, even if the directive appears mid-paragraph in
  the source. Any `///` doc-comment on the item is **excluded** from the
  rendered source body (it would recursively contain the directive that
  triggered the render).

`kio doc check` **validates** the directives: it resolves the `term`
against the active scope and reports an error if it cannot be resolved
(and, for `@type`, if `term` names a type-level name — see § Directive
keywords); no rendering takes place. `kio doc build` **renders** them
— `@signature` and `@type` to an inline code span, `@source` to a
fenced block — as part of producing the documentation site (see
§ Rendered output).

### Directive syntax

All three directives use the `` [`@KEYWORD term`] `` form — brackets
and backticks matching the intra-doc reference bracket/backtick syntax,
with an `@`-prefixed keyword and the term separated by whitespace:

```
directive-ref ::= "[`@" keyword ws+ term "`]"
keyword       ::= "signature" | "source" | "type"
term          ::= (any character except "`", "]", newline)+
ws            ::= " " | "\t"
```

Rules:

- **The keyword and the term are separated by one or more whitespace
  characters.** `` [`@signature foo`] `` is a directive; the keyword and
  term are distinct tokens. The leading and trailing whitespace of the
  term is trimmed — `` [`@signature   foo `] `` resolves `foo`.
- **A keyword with no following whitespace-separated term is not a
  directive.** `` [`@signature`] `` (no term) is treated as opaque prose.
- **Plain `` [`name`] `` without `@` is not a directive.** The scanner
  distinguishes the two forms by the `@` prefix.
- **Markdown inline/reference links are skipped.** `` [`@signature foo`](url) ``
  is a Markdown inline link and not validated as a directive.

The space-separated `keyword term` argument shape is identical to the
`kio repl` meta-commands' `:KEYWORD term`; after the `@` / `:` sigil,
the two surfaces' directive syntax is the same.

### Directive keywords

Three directive keywords are defined:

- **`signature`** — embed the item's declaration header (header line,
  no body).
- **`type`** — embed the type of the item's bound value.
- **`source`** — embed the item's full source (header + body, minus its
  doc-comment).

**`@type` against a type-level name is a contract error.** A `type` /
`labels` / `newtype` declaration binds a *type*, not a value, so it has no
bound-value type to render. `@type` against one is a Kiodoc contract
error (exit 70, `DocError`); the message names the kind found and what
`@type` expects — e.g. `` `Cnf` is a type alias; `@type` expects a
value binding (fn, host fn, exported fn) ``. The error fires on the
*type name*; a type's constructor and projector are value-level
functions and still resolve, so `` [`@type Box.mk_box`] `` against a
`newtype`'s constructor is fine. `@signature` and `@source` accept
type-level names — only `@type` rejects them.

**Unknown directive keywords are runner errors.** Any `` [`@KEYWORD term`] ``
where `KEYWORD` is not `signature`, `source`, or `type` produces a
Kiodoc contract error (exit 70, `DocError`). This catches typos
(`@signaure`, `@eval`) at the first opportunity rather than silently
doing nothing.

`@eval` is not a directive keyword and is permanently excluded from this
family.

### Term resolution

The `term` argument is resolved with the same rules and scope as the
`` [`name`] `` intra-doc reference:

- **Inside a `///` doc-comment** (`*.kio` source): the surrounding
  module's scope (top-level declarations, imports, qualified aliases,
  label entries, newtype constructor / projector, operator token sequences).
- **Inside a `.md` file**: the package boundary declared in the nearest
  enclosing `<name>.pkg.kio` file.

The same two-form grammar applies: simple name or dotted qualified path.

Override semantics also apply: a Markdown reference-link definition
(`[term]: url`) in the document suppresses validation for that key.

### What `@signature` renders

The signature is the item's declaration header in the pretty-printed form
`kio fmt` would emit, minus the body:

- **`fn`**: `fn name[type-params](value-params) -> ReturnType`
- **`type`**: `type Name = Type;`
- **`literal`**: `literal name = value;`
- **`labels`**: `labels Name = { tag1: T, tag2: U, … };`
- **`newtype`**: `newtype Name : Type { constructor c; projector p; };`
- **`op`**: `op pattern { impl function; };`
- **`host fn`** / **`exported fn`** (at the package boundary): the
  declaration header.

A member of a bare recursive type group renders the complete written
`rec { ... }` declaration, with all members in source order and all attached
doc comments excluded. The group is the declaration's required recursive
scope; displaying only the selected member would change its meaning. The
selected name still owns its own anchor, kind, and prose. No unrelated module
declarations are added to this context.

The generated uppercase nominal of `labels` is also a type-oriented
documentation target. Its signature and source show the owning labels
declaration or complete enclosing recursive group, not a synthetic standalone
newtype. This includes ordinary and recursive labels. Reuse markers introduce
no new nominal documentation target.

A nonminting forwarding label's braced selector renders its own
`type {field} = {target};` declaration for both `@signature` and `@source`,
not the target's declaration. Its own visibility and documentation determine
its page entry; forwarding introduces no nominal documentation section.
It has no bound-value type, so `@type {field}` is rejected.

### What `@type` renders

The type of the item's bound value, in the pretty-printed form `kio
fmt` would emit for a type — no `fn` keyword, no item name, no
value-binder names. `@type` resolves only against the value-binding
kinds:

- **`fn`**: the function type — e.g. `Int -> Int` for
  `fn double(n: Int) -> Int`, `[A] A -> A` for `fn id[A](x: A)
  -> A` (universal binders render with brackets, the same modal-logic
  convention `@signature` uses).
- **`host fn`** / **`exported fn`** (at the package boundary): the
  function type of the declaration.

A `type` / `labels` / `newtype` is a type-level name and is rejected
(see § Directive keywords) — it has no bound-value type. A `literal`
alias declaration is rejected until it is used in an expression with enough context,
because the declaration itself has no standalone bound-value type.

### What `@source` renders

The full item source, including the signature header and the body, with
any attached `///` doc-comment **excluded**. The output is a fenced code
block with language tag `kio`.

Recursive type members retain the same complete group context as `@signature`.
This rendering is shared by declaration hover and the REPL's `:signature`,
`:source`, and signature portion of `:doc`; a member's hover and `:doc` prose
remain its own. Completion documentation contains the selected declaration's
prose without a declaration-source block.

A `host fn` has no body, so its source is just the declaration line; an
`exported fn` (a `pub fn`) renders its full source, header and body, like
any other `fn`. The output is wrapped in a fenced block either way.

### Directive diagnostics

An unknown directive keyword produces:

```
<file>:<line>:<col>: unknown Kiodoc directive `@KEYWORD`: expected `@signature`, `@source`, or `@type`
```

An unresolved directive term produces (consistent with the intra-doc
reference diagnostic format):

```
<file>:<line>:<col>: unresolved `@KEYWORD` directive: `term` is not in scope
```

When a close name exists in the active scope (edit distance ≤ 2), the
diagnostic appends:

```
; did you mean `suggestion`?
```

A `@type` directive against a type-level name produces:

```
<file>:<line>:<col>: `term` is a type alias; `@type` expects a value binding (fn, host fn, exported fn)
```

The kind phrasing varies by what `term` resolves to — "a type alias",
"a `labels` declaration", "a `newtype`", or "a `host type`".

## Rendered output

`kio doc build` validates the package (as `kio doc check`) and then
renders a **per-module documentation site** from the combined `.md` +
`///` content. HTML is the default output; Markdown is an opt-in second
format. Both formats render the same site structure; only the page
chrome differs.

### Site structure

A doc comment immediately before a bare recursive type group's `rec` keyword
belongs to the unnamed group. It is validated like any top-level declaration
doc comment, including references and Kio fences. Module pages show it once
under **Recursive group**, immediately before that group's member sections;
it creates no resolvable symbol and is not copied into member documentation.
Hovering the group's `rec` keyword shows this prose with the complete group
declaration. Each member's own doc comment is validated independently,
including newtype and labels members.

Package-page references and directives may select a recursive type member
only when that member is exported. Keeping peer declarations as necessary
rendering context does not make their names package-boundary targets.

The site has one page per module, plus standing pages:

- **Index** — the package's top-level page. Lists the modules (each with
  the first paragraph of its module-level `///` doc-comment as a
  summary), links to the package-boundary page, and links to the rendered
  tutorial / guide pages.
- **Module pages** — one per `.kio` module file. The page leads with the
  module-level doc-comment (heading and body), then a per-item section
  for each top-level `fn` / `type` / `literal` / `labels` / `op` declaration, in
  source declaration order. Each section shows the item's signature —
  the same form `` [`@signature term`] `` embeds — followed by the
  rendered doc-comment body. Anonymous declarations have no section of their
  own; a labels block's generated nominals have sections under their type names.
  `equiv` and variadic `op` have no documentable name and are omitted.
- **Package Boundary page** — the contract surface the package's
  `<name>.pkg.kio` `bridge { … }` block selects: the `host` items
  (what the host must supply) and the other `pub` items (what the host
  may invoke) of the bridged modules, rendered the same per-item way
  as a module page's items. Each selected declaration keeps its declaring
  module's context for its own doc comment, including private references.
  Required recursive peers remain displayed source context, not additional
  selectable boundary declarations.
- **Tutorial / guide pages** — the markdown source tree under the build
  file's `docs.md` is rendered to a mirror tree under the output
  directory; filenames and directory structure carry over.

A sidebar on every page navigates between the index, the package-boundary
page, the per-module pages, and the rendered tutorial tree.

### URL and anchor scheme

- A module declared as `foo/bar/baz` renders to the page
  `foo/bar/baz.html` (or `.md`).
- Each item in a module page or the package-boundary page has a stable in-page
  anchor: `#KIND-ENCODE(MODULE)-ENCODE(NAME)`. `MODULE` is the complete declared
  module path, including `/` separators. `KIND` is `labels` for a named labels
  declaration, `op` for an operator, and `item` otherwise. `NAME` is the source
  identifier, except that operators use their complete canonical operator name
  (`op - __` for prefix minus, `op _ - __` for binary minus), not their display token alone.
  `ENCODE` retains ASCII letters and digits and encodes every other UTF-8 byte
  as `_hh`, with two lowercase hexadecimal digits. For example, `Box` in
  `pkg/left` has `#item-pkg_2fleft-Box`. Both page classes use the same fragment.
- An intra-doc reference `` [`name`] `` that resolves to an in-package
  documentable declaration links to its declaring module page and exact
  identity fragment. A same-spelling declaration in another module cannot
  replace an already selected target.

### Resolved-link and directive rendering

The renderer rewrites the validated reference data into the output:

- `` [`name`] `` → a link to the documenting page + anchor when `name`
  selects a documentable in-package declaration with a site entry; a plain
  code span otherwise. An admitted boundary role or label name without a
  declaration view remains plain code, rather than borrowing a same-spelling
  declaration's link. External source
  references render as a plain code span (no anchor) until rendered output
  has a cross-source linking contract.
- `` [`@signature term`] `` → an inline code span carrying the
  pretty-printed declaration header.
- `` [`@type term`] `` → an inline code span carrying the
  bound-value type.
- `` [`@source term`] `` → a fenced ` ```kio ` code block on its own
  line, block-promoted out of any surrounding paragraph.

A reference or directive term overridden by a Markdown reference-link
definition (`[name]: url`) is left exactly as the author wrote it — the
explicit link wins, the same override semantics `kio doc check` honors.

### Open-world safety in rendering

Every page URL and every item anchor is keyed by **source identity** —
the module path and the declaration's complete name and anchor kind — never by what else
the package contains. Adding a declaration to a module body adds a page
section and an anchor; it never relocates an existing one. The renderer
introduces no resolution that could break the open-world guarantee.

### Static assets

The HTML site is self-contained: `kio doc build --html` ships a default
stylesheet and a sidebar script under an `_assets/` directory in the
output, with no external dependency. An author who places a
`docs/_assets/` directory in the markdown source tree overrides the
built-in defaults file-for-file.

Kio code is highlighted by `kio doc` itself, not by an external
highlighter. `kio doc build --html` emits per-token
`<span class="kio-…">` markup inside both `language-kio` fenced code
blocks and `.sig` item-signature blocks. Each span's class is the
classifier's dotted wire string (`entity.name.function`,
`keyword.elaborator`, …) rewritten to a hyphenated, `kio-`-prefixed CSS
class (`kio-entity-name-function`, `kio-keyword-elaborator`, …);
inter-token whitespace is emitted verbatim, and a block that fails to
lex degrades to plain escaped text. The default stylesheet themes these
classes through `--kio-tok-*` CSS custom properties, with a light
palette under `:root` and a dark palette under
`@media (prefers-color-scheme: dark)`. The dotted-to-hyphenated CSS
class scheme is the stable rendering contract an external site
stylesheet targets.

## Overview

**Kiodoc** is GitHub-flavored Markdown plus a small set of
directives that let Kio source blocks inside markdown files be
validated and paired with expected output fences. The directives
ride as Pandoc-style attribute lists on a fence (`{...}` after
the language tag) or, when an author wants the directive hidden
from GitHub readers, inside an HTML comment
(`<!--LANG {attrs} ... -->`). A Kiodoc file is still readable on
GitHub: the attribute lists render visibly inside the fence info
string but don't affect highlighting, and the HTML comments don't
render at all.

Three consumers of this contract:

1. **The kio CLI's `kio doc` subcommands** — `kio doc check`
   validates a package's Kiodoc content against this contract;
   `kio doc fmt` canonicalizes formattable Markdown snippet bodies
   using this contract's harness and variant semantics; `kio doc
   build` validates and renders to a documentation site (see
   § Rendered output). Tutorial authors run them locally; CI runs
   `kio doc check` and `kio doc fmt --check` on `docs/`.
2. **The website's docs pipeline**
   (`website/scripts/prepare-docs.mjs`) — runs `kio doc build
   --html` and folds the rendered pages into the published site.
   It consumes the rendered output (see § Rendered output) rather
   than parsing directives itself, so its interpretation of a
   snippet is the CLI's by construction.
3. **CI's `ci/checks/orchestrators/kiodoc-tests.sh`** — runs the
   `kio doc` goldens and invokes `kio doc check` plus
   `kio doc fmt --check` on `docs/` on every CI run, failing the
   run on any error.

One directive interpretation everywhere: a consumer either runs
`kio doc` itself (1, 3) or consumes its rendered output (2). If
any two consumers diverge on what a snippet means, that's a bug
in one of them.

The runner's parser does *not* skip past HTML-comment content —
comments can carry directive-bearing material that participates in
the same machinery as visible fences. But the parser only *acts*
on comments that exactly match the hidden-fence shape
`<!--LANG {attrs}\n<body>\n-->`. Anything else — ordinary prose
comments (`<!-- TODO: rewrite -->`), near-miss typos of the
hidden-fence shape — is treated as opaque prose. A mistyped
hidden-harness declaration therefore surfaces later as a "no such
harness" error or a missing-output-fence error rather than as a
comment-parser error; that's an acceptable failure mode.

## What `kio doc check` validates

- **Attributes:** `harness=NAME`, `placeholder=JSON_STRING`
  (harness insertion marker), `file`, `accumulate`, `@NAME` (wrap in
  harness), `placeholder=JSON_OBJECT` (snippet placeholder
  substitutions), `ignore`, `variant=KIND`, `check_exit_code=N`,
  `run_exit_code=N`, `stdout`, `stderr`. `check_exit_code` is
  asserted. The run-trigger attributes (`stdout`, `stderr`,
  `run_exit_code`) are recognized structurally: pairing rules are
  enforced, but `kio doc check` does not execute snippets or compare
  runtime output.
- **Named harnesses.** Every harness has a name. A `kio` snippet
  either references one with `{@NAME}` (wrap in harness), carries
  `{ignore}` (skip), or does neither — in which case it is
  **standalone**: the snippet body is itself a complete Kio
  program, fed to `kio check` as-is.
- **Document-scoped files.** A `kio {file}` fence contributes a
  support file to file-backed snippets in the document. The file path is
  inferred from the body's Kio file header.
- **Package root module support files.** Markdown module snippets
  are checked in scratch packages that also include the `*.kio`
  files directly reached by the assembled program's `import`
  imports. The candidate set is the package root
  plus any `docs { support "<dir>"; }` directories declared in the
  package build block. Support directories are validation-only:
  their files are available to snippets, but are not rendered as
  documentation content. This lets snippets import same-package or
  shared imported elaborators exactly as ordinary modules do
  without injecting unrelated root modules into the scratch
  package. Module identities are their full relative slash paths,
  including subdirectories. Discovery excludes nested packages,
  hidden directories, `out`, and `target`, and does not follow
  directory symlinks. The package root precedes the configured support
  directories in their listed order; the first location providing a
  top-level module namespace owns that namespace and its descendants.
  Document-scoped files and file-backed harness files take precedence
  over support files in their top-level module namespace. Their explicit
  transitive import closure is included too. Selection follows written
  module imports, not declarations discovered in an unrelated module.
- **Hidden form.** Any fence — harness, file, snippet, or output —
  can be wrapped in an HTML comment so GitHub readers don't see it.
- **Snippets:** every `kio` fence not marked `{ignore}` is
  validated. Module, harness-wrapped, and file-backed snippets are
  assembled into a scratch package and checked with `kio check`.
  Non-module `variant=KIND` snippets are routed through the parser
  for that file kind (`package`, `signature`, `dependency`, or
  `lock`). If `{check_exit_code=N}` is set,
  the runner asserts the check/parser exits with code N; otherwise
  it asserts exit 0.
- **Validator:** module-bearing snippets use `kio check` (per
  [`cli.md` § `kio check`](cli.md)) — parse, resolve, typecheck,
  no codegen. Non-module variant snippets use the corresponding
  parser only. The CI job fails on any non-zero exit (modulo
  `{check_exit_code=N}`).
- **Run-trigger attributes recognized structurally.** `stdout`,
  `stderr`, and `run_exit_code=N` declare output/exit expectations
  for renderers and other consumers. `kio doc check` enforces pairing
  for `stdout` / `stderr`, but does not execute the snippet.
- **Substitution:** with `{@NAME}`, the snippet body replaces the
  harness's declared placeholder, preserving the snippet's own
  indentation. A snippet-level `placeholder=JSON_OBJECT` rewrites
  display-only placeholder text to validation text before harness
  substitution or standalone validation. If the harness carries
  `file`, the substituted harness body is validated as a real Kio
  file alongside the document-scoped support files. Without
  `{@NAME}` (standalone), the snippet body is fed to `kio check`
  directly.
- **No forward references.** A harness must be declared above any
  snippet that references it. Out-of-order references are a
  runner error.
- **Unknown attributes are runner errors.** A `kio` fence carrying
  an attribute the runner doesn't recognize (typo'd no-value flag,
  unknown key, unknown harness name) fails immediately. Catches
  typos like `{@main stout}` instead of `{stdout}` at the first
  opportunity, rather than letting the snippet silently miss its
  pairing.
- **Diagnostics map back to markdown source.** When `kio check`
  fails on a synthesized program, the error message references the
  markdown file and line of the originating snippet, not the
  synthesized intermediate. Authors see `tutorials/intro.md:42`,
  not a temp-file path. On failure, `kio doc` also prints the
  assembled source or assembled file set for the failing snippet
  (after harness substitution) so the author can see what was
  actually compiled.
- **Non-`kio` fences are ignored,** except output fences carrying
  `{stdout}` or `{stderr}` paired with a preceding snippet (see
  § Patterns).

## What `kio doc fmt` formats

`kio doc fmt` formats Kiodoc snippets in Markdown files under the
package's configured `docs.md` tree. It is not a Markdown mode of
`kio fmt`: it uses the Kiodoc document model so formatting sees the
same virtual source shape that `kio doc check` validates.

For each non-ignored `kio` snippet, the formatter applies snippet
placeholder substitution, harness substitution, file-backed harness
assembly, accumulating-harness aggregation, and `variant=KIND`
routing before parsing. It then formats the assembled virtual Kio
file with the normal parser and pretty-printer for that file kind.
The visible Markdown fence body is rewritten only when the formatted
virtual source maps cleanly back to that body. If the snippet is
invalid or the mapping is ambiguous, `kio doc fmt` reports a Kiodoc
diagnostic and leaves the file unchanged.

`kio doc fmt` skips ignored snippets, output fences, opaque non-`kio`
fences, and `///` doc-comments. The doc-comment surface is validated
and rendered by `kio doc check` / `kio doc build`, but it is not a
Markdown file and is not rewritten by `kio doc fmt`.

## Patterns

In the examples below, assume a harness named `main` has been
declared earlier in the document.

### Snippet only

A `kio` fenced block, compiled within the referenced harness.

````markdown
```kio {@main}
let x = 1 + 1
```
````

### Standalone snippet

A snippet that is itself a complete standalone program — a package
declaration with its `bridge { … }` block followed by the
module(s) it admits, in one block. `{}` (the empty
attribute list, with no `@NAME` and no `ignore`) marks the snippet
as standalone; the body is fed to `kio check` as-is.

````markdown
```kio {}
package tutorial_intro;

bridge {
  tutorial_intro;
  tutorial_intro/**;
}

module tutorial_intro;

host type I32 role(i32);
host fn add(a: I32, b: I32) -> I32;

module tutorial_intro/main;

import tutorial_intro(add);

pub fn main() -> . { let _ = add(1, 1); () }
```
````

### Snippet to skip

`{ignore}` tells the runner to leave the snippet alone — no
validation, no pairing checks. It is the last-resort form for
snippets that are not complete programs (syntax fragments) or
that do not fit any harness, `file`,
`variant=KIND`, `check_exit_code=`, or placeholder-substitution
shape in the document. It may combine with `variant=KIND` to label
a skipped package-file fragment.

````markdown
```kio {ignore}
List<Int>     // type fragment, not a runnable program
```
````

### Snippet with display placeholders

A snippet can show placeholder prose while compiling replacement
text. The `placeholder=JSON_OBJECT` value maps exact visible text
to exact validation text. Multiple mappings are supported.

````markdown
```kio {@main placeholder={"...":"dummy_code()","???":"fallback()"}}
fn first() -> String { ... }
fn second() -> String { ??? }
```
````

The rendered snippet still shows `...` and `???`; `kio doc check`
validates the body produced by replacing them with `dummy_code()`
and `fallback()`.

### Snippet + expected stdout

The snippet declares `{stdout}`. A later fence carrying `{stdout}`
is the expected standard-output text. Prose can appear between the
two, but no other fence (code block of any language) may
intervene.

````markdown
```kio {@main stdout}
print (1 + 1)
```

The program prints:

```text {stdout}
2
```
````

The website renders the pair as code → output with a Run button on
the snippet. `kio doc check` enforces the pair's structure.

### Snippet expected to fail check

Demonstrates a failure category at typecheck time without
committing to specific message text. The runner asserts
`kio check` exits with the named code (see
[`exit-codes.md`](exit-codes.md)).

````markdown
```kio {@main check_exit_code=22}
let .(x: Int) = "not a number"
```
````

### Snippet + expected stderr

Composes `check_exit_code=N` with `stderr` to assert both the
failure category *and* the exact diagnostic text in a paired
`{stderr}` fence. `kio doc check` enforces the pairing.

````markdown
```kio {@main check_exit_code=22 stderr}
let .(x: Int) = "not a number"
```

Producing:

```text {stderr}
type error: expected Int, got String
```
````

### Snippet + expected stdout *and* stderr

Both attributes can apply to one snippet. The output fences may
appear in either order; no other fence may intervene. Presence of
either (or `run_exit_code=N`) records an output/exit expectation for
renderers and other consumers.

````markdown
```kio {@main stdout stderr}
print (1 + 1)
warn "deprecated"
```

```text {stdout}
2
```

```text {stderr}
deprecation warning: ...
```
````

### REPL session

A single `kio-repl` fenced block where lines starting with the
prompt `>>>` (followed by a space) are input and the rest is
expected output. Convention borrowed from Python doctest, IPython,
GHCi tutorials. `kio doc check` treats `kio-repl` fences as opaque
prose.

````markdown
```kio-repl
>>> 1 + 1
2
>>> let x = 5
>>> x * 2
10
```
````

CI feeds each input line through a REPL and asserts the in-between
output. The website renders each `>>>` line as an editable input
that re-runs on change.

## Harness

The harness is the program skeleton — a package declaration with
its `bridge { … }` block, the module(s) carrying the host
declarations and any shared definitions, and a declared placeholder
marking where the snippet drops in. A document can declare one or
more.

### Declaration (visible)

A harness is declared as a `kio` fence carrying `harness=NAME`
and `placeholder=JSON_STRING`. The JSON string names the exact
marker text in the harness body; that marker must appear exactly
once. Each named harness must be unique within the document —
redeclaring a name is a runner error.

````markdown
```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
package tutorial_example;

bridge {
  tutorial_example;
}

module tutorial_example;

host type I32 role(i32);
host fn add(a: I32, b: I32) -> I32;

__INSERT_CODE_HERE__
```
````

A second harness, for snippets that need different host
capabilities:

````markdown
```kio {harness=with_io placeholder="__INSERT_CODE_HERE__"}
package tutorial_example;

bridge {
  tutorial_example;
}

module tutorial_example;

host type I32 role(i32);
host type String role(str);
host fn add(a: I32, b: I32) -> I32;
host fn print(s: String) -> .;

__INSERT_CODE_HERE__
```
````

### Hidden form (any fence)

Any fence — harness, file, snippet, or output — can be hidden from
GitHub readers by wrapping it in an HTML comment using the form
`<!--LANG {attrs}\n<body>\n-->`:

```
<!--kio {harness=main placeholder="__INSERT_CODE_HERE__"}
package tutorial_example;

bridge {
  tutorial_example;
}

module tutorial_example;

host type I32 role(i32);
host fn add(a: I32, b: I32) -> I32;

__INSERT_CODE_HERE__
-->
```

The runner parses the inner `LANG {attrs}` line identically to a
real fence. Kio uses `//` for line comments, so embedding Kio
source inside an HTML comment doesn't collide with the comment
terminator. For non-Kio bodies (e.g. a `text` output fence), avoid
`--` in the body — it's reserved by HTML comment syntax and can
confuse consuming tooling.

### Reference

A snippet picks its runner mode by what its attribute list
contains:

````markdown
```kio {@main}
add(1, 1)                      // wrap in harness "main"
```

```kio {@with_io}
print("hi")                    // wrap in harness "with_io"
```

```kio {}
package tutorial_intro;     // {} = standalone; snippet is its own complete program
bridge { tutorial_intro; }
module tutorial_intro;
host type I32 role(i32);
host fn add(a: I32, b: I32) -> I32;
```

```kio {ignore}
List<Int>                      // skip entirely
```
````

At most one `@NAME` attribute may appear per fence. With `@NAME`,
substitution is literal: the runner replaces the harness's declared
placeholder in the harness body with the snippet body. Without
`@NAME` (and without `{ignore}`), the snippet body is the full
input to `kio check` — no substitution.

### Substitution position

The position of the declared placeholder inside the harness body
determines how the runner parses a substituted snippet. The runner
inspects the harness template once when the harness is declared
and classifies the marker's position as one of two kinds:

- **Top-level position** — the marker sits at module scope: outside
  every `{...}` group, between top-level items. A snippet member
  contributes top-level forms (`fn`, `type`, `literal`, `labels`,
  `newtype`, `op`, `import`, `pub fn`).
- **Block position** — the marker sits inside a `fn` body's
  `{...}` group at any nesting depth. A snippet member contributes
  block contents: `let` statements, expression statements, and a
  trailing expression — the same shape a `fn` body accepts.

Authors choose between the two by where they place
the placeholder. Wrapping it in `pub fn main() -> . { … }`
makes the harness admit expression-shaped snippets; placing it
between top-level items makes the harness admit top-level forms.
The two positions can coexist in one document via separate
harnesses.

The classification is keyed off the literal text of the harness
template — brace-balance between the harness start and the marker.
The runner does not consult any other state when deciding.

## Fence attributes

A fence's attribute list — Pandoc-style `{...}` after the language
tag — carries the directive vocabulary. Multiple attributes are
separated by whitespace.

### Grammar

The list appears on the same line as the language tag, separated
from it by a single space:

```
attr-list   := "{" ws* ( ref (ws+ attr)* | attr (ws+ attr)* )? ws* "}"
ref         := harness-ref | self-ref
attr        := flag-attr | kv-attr
flag-attr   := name
kv-attr     := name "=" value
harness-ref := "@" name
self-ref    := "@"
name        := _? [a-z]+ [0-9]* ( '_' [a-z]+ [0-9]* )* _*
value       := integer | name | json-string | json-object
integer     := [0-9]+
json-string := JSON string literal
json-object := JSON object literal
ws          := " " | "\t"
```

Rules:

- **Four lexical forms.** Plain flag (`stdout`, `ignore`),
  key-value (`check_exit_code=22`), `@`-prefixed harness reference
  (`@main`), and the bare `@` self-reference — an `@` no name
  follows. The form is decided lexically; no closed
  reserved-vocabulary lookup is needed to parse. Whitespace never
  separates `@` from its name: `{@ main}` is a bare `@` beside the
  flag `main`, not `{@main}`.
- **The harness reference opens the list.** `@NAME` and the bare `@`
  are both harness references; when a fence carries one it is the
  first attribute. `{@main check_exit_code=22}` is the spelling;
  `{check_exit_code=22 @main}` and `{check_exit_code=22 @}` are
  runner errors.
- **At most one harness reference per list.** `{@main @other}`,
  `{@ @main}`, and `{@ @}` are runner errors — a snippet wraps in at
  most one harness. A list carrying two reports the duplication
  rather than the second one's position.
- **The bare `@` is a doc-comment form.** It names the snippet's
  surrounding module as its harness, so it is a runner error on a
  `.md` fence, which has no surrounding module. See
  § Doc-comment input surface.
- **Order is irrelevant among flags and key-value attributes.**
  `{@main stdout check_exit_code=22}` and
  `{@main check_exit_code=22 stdout}` mean the same thing. Only the
  harness reference has a fixed position.
- **No repeated names.** `{stdout stdout}`, `{harness=a harness=b}`
  are runner errors.
- **Context decides the value form.** `harness=NAME`, `variant=KIND`,
  and numeric attributes still require unquoted identifier/integer
  values. Only `placeholder=JSON_STRING` and
  `placeholder=JSON_OBJECT` consume JSON values.
- **JSON values stay inside one attribute.** JSON string/object
  values are parsed as one value even when they contain JSON
  punctuation. Attributes are still separated by whitespace; there
  are no commas between attributes.
- **Unknown attributes are runner errors.** Any unrecognized flag
  name, any unknown key in `key=value` form, or any `@NAME`
  referencing an undeclared harness fails immediately. Catches typos at the first opportunity instead of
  letting a snippet silently skip its pairing or validation.
- **An attribute-less ` ```kio ` fence is a runner error.**
  Authors must write at least `{}` to make the choice explicit.
  `{}` itself is the empty attribute list — no `@NAME`, no
  `ignore` — meaning the snippet is standalone.

Names (in any of the three forms) follow Kio's value-identifier
rule
([`language.md` § Naming conventions](language.md)):
lowercase ASCII words whose digits follow their letters, separated by one
underscore, with at most one leading underscore and any trailing underscores.
Picking the same shape as Kio source means a harness name like
`with_io` looks identical whether it's used as `{@with_io}` in
attribute position or referenced inside Kio source.

### Vocabulary

**On a `kio` fence — declares a harness:**

- **`harness=NAME`** — declare a named harness. The fence body is
  the harness template and must include the marker named by
  `placeholder=JSON_STRING` exactly once.
  Each name may be declared only once per document; redeclaration
  is a runner error. A harness-declaration fence carries
  `placeholder=JSON_STRING` and may carry `accumulate` or `file`;
  no other attributes are permitted on a harness declaration.
- **`placeholder=JSON_STRING`** — required on a harness declaration.
  The decoded string is the literal insertion marker in the harness
  body. It must be non-empty and must occur exactly once.
- **`file`** — no-value flag on a harness-declaration fence. The
  harness body is a complete Kio file template. The runner infers
  its scratch path from the first Kio file-header clause in the
  body, then substitutes each referencing snippet at the declared
  placeholder and validates that file together with the document's
  support files. `file` is mutually exclusive with `accumulate`.
- **`accumulate`** — no-value flag on a harness-declaration fence. When
  set, every `{@NAME}` snippet referencing this harness is treated
  as a *member* of one aggregate program rather than as an
  independent snippet. The runner concatenates the member bodies in
  document order, substitutes the concatenation into the harness
  once, and runs `kio check` against the aggregate. See § Accumulating
  harnesses below.

**On a `kio` fence — document-scoped support file:**

- **`file`** — with no other attributes, declare a support file
  available to file-backed harnesses in the document. The body must start
  with a Kio file-header clause; the runner infers the scratch path
  from that header. See § Document-scoped files.

**On a `kio` fence — snippet behavior:**

- **`@NAME`** — wrap this snippet in the named harness. Without
  `@NAME` (and without `ignore`), the snippet is **standalone**:
  it's expected to be a complete Kio program by itself, fed to
  `kio check` as-is.
- **`ignore`** — skip the snippet entirely. The runner does not
  engage it. It may combine only with `variant=KIND`.
- **`placeholder=JSON_OBJECT`** — replace display-only placeholder
  text before validation. Each object key is exact text that must
  occur in the visible snippet body; each value is the exact text
  compiled by `kio doc check`. The object must be non-empty, keys
  must be non-empty, and values must be strings. Multiple mappings
  apply to the original snippet body in one pass; replacements do
  not cascade into later replacements. Longer keys are matched
  before shorter overlapping keys.
- **`variant=KIND`** — route the snippet body through the parser
  for the named file kind rather than through the regular-module
  parser. `KIND` is one of `module` (the default — explicit
  spelling, equivalent to omitting the attribute), `package`,
  `signature`, `dependency`, or `lock`.
  A `variant=` snippet is always standalone: it carries no `@NAME`
  and cannot be a harness member. See
  § Variant snippets below.

**On a `kio` fence — check-time assertion:**

- **`check_exit_code=N`** — assert `kio check` exits with code N
  (per [`exit-codes.md`](exit-codes.md)).
  Default when omitted is `0`.

**On a `kio` fence — run-trigger attributes:**

Presence of any one of these marks the snippet as "to be run."
`kio doc check` recognizes them structurally and enforces pairing
for `stdout` / `stderr`, but does not execute snippets.

- **`stdout`** — a `{stdout}` output fence appears later in the
  document, paired with this snippet.
- **`stderr`** — same for `{stderr}`.
- **`run_exit_code=N`** — assert the *running* snippet exits with
  code N. `kio doc check` checks the attribute is well-formed.

**On a `text` (or other) fence — paired output:**

- **`stdout`** — this fence is expected standard-output text for
  the most recent snippet declaring `{stdout}`.
- **`stderr`** — this fence is expected stderr text for the most
  recent snippet declaring `{stderr}`.

The shared `stdout` / `stderr` attribute names mean two related
things depending on the fence's language tag: on a `kio` fence, "I
expect such-and-such an output fence"; on a non-`kio` fence, "I am
that output fence." The language tag on the output fence is
conventionally `text` but is not validated — `json` (when the
snippet prints JSON), `console`, `txt`, `output`, or any other tag
works equally well; only the attribute matters to the runner.
Authors pick whatever tag gives the rendering they want on GitHub.

### Pairing rules

A snippet declaring `{stdout}` and/or `{stderr}` must be followed
by the corresponding output fence(s) — *before any other fence
appears*. Prose between them is fine; another code fence is not.
If both are declared, the two output fences may appear in either
order. An output fence with no preceding snippet declaring the
matching attribute is an orphan and is a runner error.

### Check vs. run

`check_exit_code=N` asserts the result of `kio check`
(typecheck-time). The run-trigger attributes (`stdout`, `stderr`,
`run_exit_code=N`) record execution expectations without running the
snippet. The two are independent dimensions; a snippet may carry
assertions in either, both, or neither category.

### Output matching

`kio doc check` preserves `{stdout}` / `{stderr}` fence bodies for
renderers and enforces their pairing with snippets. It does not
compare those bodies against runtime output.

## Accumulating harnesses

A harness carrying the `accumulate` flag on its declaration
builds **one** aggregate program from every `{@NAME}` snippet that
references it, in document order. The aggregate is what the runner
validates — not each member independently.

````markdown
```kio {harness=running placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

__INSERT_CODE_HERE__
```
````

A document later can introduce one piece at a time:

````markdown
```kio {@running}
fn greet(name: S) -> . { print(name) }
```

prose between members…

```kio {@running}
pub fn main() -> . { greet("world\n") }
```
````

The runner concatenates the bodies of every `{@running}` snippet
(in source order, with a newline between each) and substitutes
the concatenation into the harness once at the harness's declared
placeholder.
A single `kio check` invocation validates the aggregate.

Rules:

- **Each member's body is parsed in the harness's substitution
  position.** Top-level harness → top-level forms; block-position
  harness → block contents. A document's later members see the
  earlier members' declarations — that is the point.
- **`check_exit_code=N` is rejected on an accumulating member.**
  An accumulating harness validates as one program; a per-member
  exit-code assertion has no aggregate meaning. Use a
  non-accumulating harness when each snippet's exit code is the
  test's subject.
- **`stdout` / `stderr` / `run_exit_code=N` are rejected on an
  accumulating member.** Run-trigger attributes describe
  per-snippet execution; one aggregate has one execution.
- **Member ordering is source order.** Members in HTML-comment
  hidden fences participate too, in their source position.
- **An accumulating harness with no members produces no aggregate
  and no validation.** This is the analogue of a harness with no
  references in the non-accumulating case — it is not an error.

The non-default `variant=` snippets are never members of any
harness, accumulating or otherwise; they validate as their own
file kind (see § Variant snippets).

## Document-scoped files

A `kio {file}` fence declares a support file that belongs to the
document's scratch package for file-backed harnesses. It is
not a snippet, does not pair with output fences, and is not
validated on its own.

The fence body must start, after any leading blank lines and `//`
comments, with one Kio file-header clause. The runner infers the
scratch path from that header:

| Header form | Inferred path |
| --- | --- |
| `module pkg/main;` | `pkg/main.kio` |
| `module utils;` | `utils.kio` |
| `package pkg;` | `pkg.pkg.kio` |

Module headers may use slash-qualified module paths; a root module
names a single root-level stem. Package headers name a single
root-level stem. Two Kio file fences in the same document may not
infer the same path, whether they are `file` support fences or
file-backed harness declarations.

````markdown
<!--kio {file}
module utils/list_ops;

pub fn fold[A](x: A) -> A { x }
-->
````

The `file` flag can also appear on a harness declaration:

````markdown
<!--kio {harness=consumer file placeholder="__SNIPPET__"}
module pkg/main;

__SNIPPET__
-->

```kio {@consumer}
import utils/list_ops(fold);
```
````

A file-backed harness is substituted as a real Kio file at the
path inferred from its own header. A snippet referencing it
validates the substituted harness file together with the document's
`file` fences. If the assembled file set uses a package root but no
matching `<pkg>.pkg.kio` was supplied, the runner supplies
an empty package file for that package.

File-backed harnesses are independent per snippet. They may use
snippet-level `placeholder=JSON_OBJECT` and `check_exit_code=N`
like ordinary harnesses. They cannot carry `accumulate`, because
an accumulating harness produces one aggregate substitution while
file-backed snippets validate independent file sets.

## Variant snippets

The `variant=KIND` key-value attribute routes the snippet body
through the parser for the named file kind:

- **`variant=module`** — equivalent to omitting the attribute.
  Standalone module body; the body is fed to `kio check` as a
  regular Kio module.
- **`variant=package`** — the snippet body is parsed as a
  `*.pkg.kio` package file. See
  [`package.md`](package.md) § Package file.
- **`variant=signature`** — the snippet body is parsed as a
  `*.sig.kio` signature changelog. See
  [`versioning.md`](versioning.md) § The `kio sig` command.
- **`variant=dependency`** — the snippet body is parsed as a
  `*.dep.kio` dependency declaration. See
  [`package.md`](package.md) § Dependency files.
- **`variant=lock`** — the snippet body is parsed as a
  `*.lock.kio` dependency lock file. See
  [`package.md`](package.md) § Dependency files.

These names are the same file-kind vocabulary used by document-scoped
file-header inference.

A variant snippet is always **standalone**: it carries no
`@NAME` (it does not get wrapped in any harness), and it is never
a member of an accumulating harness. The `check_exit_code=N`
attribute composes with `variant=` the same way it composes with
the standalone module form. The run-trigger attributes (`stdout`,
`stderr`, `run_exit_code=N`) compose syntactically, but
`kio doc check` never runs a variant snippet — these files describe
a package's surface, not an executable.

The `ignore` flag composes with `variant=` the same way it composes
with the standalone form — `{variant=KIND ignore}` skips any file-kind
variant snippet entirely.

````markdown
```kio {variant=package}
package hello;

bridge {
  hello;
  hello/**;
}
```
````

`kio check`'s diagnostic for a variant snippet locates errors at
the markdown source line of the offending fence, the same way it
does for module-variant snippets.

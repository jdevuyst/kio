# Kio CLI

The `kio` command-line program drives the Kio transpiler. This document covers the implemented command surface.

For the underlying data model — `<name>.pkg.kio` and its optional `build { ... }` block — see [`package.md`](package.md).

Exit codes signal the **category** of an error (parse, type, elaborator, …) so that tests can assert implementation-agnostically; see [`exit-codes.md`](exit-codes.md) for the table.

## Global options

- `--no-cache` — Accepted by every subcommand and may appear anywhere in argv (before, between, or after positional arguments); the dispatcher strips it before the subcommand sees its arguments. Disable every persistent on-disk Kio cache for this invocation: both reads and writes. An implementation that keeps no such caches treats the flag as a no-op. The intent is to force a cold semantic run for diagnosis or benchmarking while leaving host-toolchain caches alone. The caches that fall under this contract are Kio-semantic by definition (package-check, typed-module, enriched-IR, emit/artifact, equiv, doc-snippet, …); host-toolchain caches that are content-addressed by their host inputs (e.g., `cargo`'s `target/`, `rustc`'s incremental cache, an emitted Rust crate's compiled artifacts) are outside the scope of this flag and remain on.

- `-V` / `--version` — Recognized at the top level, before any subcommand. Prints the binary name and version (`kio <version>`) and exits `0`. A build ahead of its release tag appends a `-dev (<commit>)` provenance marker — with `-dirty` when the working tree carries uncommitted tracked changes; a released build, or a build from a published crate, prints the bare version.

## `kio init [<package-name>]`

Scaffolds a new Kio package in the current directory. With no
`<package-name>`, the command uses the current directory's basename.
The package name must be a value-name identifier: each lowercase ASCII word
contains letters followed by optional digits, with one underscore between
words. One leading underscore and any trailing underscores are allowed;
`_a`, `a1_b2`, and `foo__` are valid, while `_1a`, `a1b`, `foo_123`,
`foo__bar`, and names beginning with `__` are not.

**Behavior:**

1. Determine the package name from the positional argument or current
   directory basename.
2. Reject invalid package names at the CLI-usage tier.
3. Refuse to overwrite an existing package: if `<package-name>.pkg.kio`,
   `main.kio`, or any `*.pkg.kio` file already
   exists at the root, exit with a usage error and name the existing
   file.
4. Create `<package-name>.pkg.kio` and `main.kio`.

The generated package file declares a JS build target under
`out/js/`, a package cache under `out/.kio-cache/`, and a
`bridge { main }` block selecting the root module. The generated
`main.kio` module declares a `String` host type with `role(str)`,
a `print(p0: String) -> .` host fn, and a `pub fn main` that prints
a hello-world string. No other host declarations are generated.

`kio init` accepts at most one positional package name and no flags
other than `-h` / `--help`.

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the package files were created.
- `2` — CLI usage error: invalid package name, unknown flag, too many
  positional arguments, or existing package files would be overwritten.
- `1` — internal error: current directory or filesystem operations failed.

## `kio check [<module>...]`

Runs Kio's typechecker on the current source tree and reports type errors. Does not produce build output. Purely local — no network, no fetching.

`kio check` walks the current directory's local source tree: regular `.kio`
modules, root `*.kio` files, and the package file at the root when one is
present. A `*.pkg.kio` package file is optional — `kio check` type-checks a
bare module tree, inferring the package name from the directory.

`kio check` accepts the same optional positional selectors as
[`kio test`](#kio-test): with no argument the whole package is checked;
with one or more `<module>` selectors the package is still checked and
each selector is validated against its module set. A selector is either
a **module name** in module-path form (`pkg/utils/string`) or a
**filename path** (any argument ending in `.kio`, or containing a `\`
path separator); an unknown selector exits at the CLI-usage tier (code
`2`) with a diagnostic naming the available modules. With no positional
selector, a directory containing zero `.kio` files is a CLI-usage error
(code `2`, message `no .kio files found in the current directory`) —
there is nothing to check. Cross-module resolution means a selector
scopes which names are validated, not which modules are type-checked.

## `kio build [--skip-unsupported-targets] [<target-id>...] [<package-path>...]`

Transpiles one or more packages to one or more compilation targets declared in the `build { ... }` block of `<name>.pkg.kio`.

With no arguments, every target in the build block is emitted. With one or more target ids, only the named ones are emitted; an unknown id is a hard error.

**Positional disambiguation: package path vs. target id.** A positional argument is read as a **package selector** when it names an existing path — a directory or a `*.pkg.kio` file; otherwise it is a **target id** (a bare identifier such as `js` or `rust`). The disambiguation is purely path-existence: path exists ⇒ package selector, else target id. With no package selector, `kio build` discovers every package in the current directory's subtree and builds each; with one or more selectors, it builds exactly the named packages. The two argument kinds combine freely (`kio build rust ./pkg-a ./pkg-b` builds the `rust` target of two named packages).

**Behavior:**

1. Confirm the package marker — exactly one `<name>.pkg.kio` at the package root — before walking the source tree. If absent, exit with a message pointing at how to scaffold one.
2. Read the `build { ... }` block off the parsed package file. A package file with no build block declares no compilation targets; exit at the build-error category with a message pointing at how to add one.
3. For each selected target, hand the target block's key/value map to the corresponding backend.
4. Each backend validates its keys (unknown keys are an error) and writes output under the target's `out` directory (relative to the package root).
5. `--skip-unsupported-targets` only affects implicit selection (no positional `<target-id>`). When set, a target whose backend isn't recognized by this build of `kio` is skipped instead of erroring; one warning per skip is printed to stderr in the form `warning: skipping target '<id>': no backend in this kio build`. Exit 0 if every target that did run succeeded, including the case where every target was skipped. Explicit positional ids are unaffected: an unsupported backend named on the CLI still exits with the build-error category. The flag suppresses unsupported-backend skips only — every other build-time failure exits at the build-error category (code `40` — see [`exit-codes.md`](exit-codes.md)): missing package file, a package file with no build block, duplicate target ids, key-validation errors, codegen errors, runtime audits. Unknown explicit target ids likewise exit at code `40`. (The build block is parsed as part of the source walk, so a malformed build block surfaces as a parse error from `kio check`'s pipeline at the parse-error category, not here.)

The contract for per-backend output layout and incremental behavior is backend-specific and documented alongside each backend in [`specs/backends/`](backends/) ([`backends/js.md`](backends/js.md), [`backends/rust.md`](backends/rust.md)).

## `kio test [<module>...]`

Discharges every [`equiv`](language.md#equivalence-claims-equiv) declaration in the current source tree by partial-evaluating each arm body and comparing the residual normal forms. A `*.pkg.kio` package file is optional — `kio test` discharges equivs in a bare module tree as well as in a package, mirroring `kio check`. The formal account of the reduction strategy and equivalence relation lives in [`specs/formal/equiv.md`](formal/equiv.md).

With no positional argument, every module in the current directory is discharged. With one or more `<module>` selectors, only the matching modules' equivs run. A selector is either a **module name** in module-path form (`pkg/utils/string`) or a **filename path** (any argument ending in `.kio`, or containing a `\` path separator); unknown selectors exit at the CLI-usage tier (code `2`) with a diagnostic naming the available modules. With no positional selector, a directory containing zero `.kio` files is a CLI-usage error (code `2`, message `no .kio files found in the current directory`) — there is nothing to test. This is distinct from step 5's zero-`equiv` case below: a directory that *has* `.kio` source but declares no `equiv` blocks still prints `no equiv blocks found` and exits `0`.

**Dependency modules are skipped by default.** A [dependency](package.md#dependency-files) materializes its modules under the `<local>/…` re-rooted root (the dependency's local name becomes a synthetic leading module segment — see [`package.md` § Dependency files](package.md#dependency-files)). `kio test` discharges only the **current package's own** equiv blocks — every module whose leading path segment is *not* a declared dependency's local name — so testing a dependency's internal laws stays the dependency author's job, not the consumer's. Pass `--include-deps` to discharge the dependency modules' equiv blocks as well. When the default skips one or more dependency equiv blocks, `kio test` prints a one-line note to **stderr** (stdout, which the result line and any failure detail go to, stays the consumer's own blocks only): `note: <N> equiv block(s) in dependency modules skipped; pass --include-deps to include`. The note is suppressed when nothing is skipped (the package declares no dependency, or `--include-deps` was given). A `<module>` selector still composes with the default: a selector that matches a dependency module discharges it only under `--include-deps`.

**Behavior:**

1. Run the same parse / use-resolution / name-resolution / type-check pipeline as `kio check` (a `*.pkg.kio` package file is optional; when present it is parsed and validated). Compile-time failures surface their own category exit code per [`exit-codes.md`](exit-codes.md); the runner reports nothing past that point.
2. Walk every typechecked module that is in scope — the modules matching the `<module>` selectors (or all of them when none is given), minus the dependency modules unless `--include-deps` was passed — and pick out each `Item::Equiv`.
3. For each equiv, evaluate every arm body via the host-independent partial evaluator and group the residual normal forms by α+η-equivalence (see [`specs/formal/equiv.md`](formal/equiv.md) § 4). Parametric equivs run too: type parameters are erased at evaluation time; each value parameter is bound, once per equiv block, to a fresh opaque atom shared across every arm so equal references on different sides compare equal. An executed structural-recursion totality fault in an arm or in an α/η closure-comparison probe aborts discharge before normal-form grouping and reports the Totality category. When the package enables `build { cache "<path>"; }`, an implementation may reuse a fresh per-equiv cached result instead of re-running the evaluator; the printed result and exit code are identical to a fresh discharge, stale or malformed cache entries are ignored, and a totality-fault outcome is never stored as an ordinary equiv result.
4. Print one line per equiv to stdout, indented two spaces: `` pass equiv `<name>` in <module> `` for a single NF group, or `` fail equiv `<name>` in <module>: arms split into <N> normal-form groups `` followed by one further-indented (four spaces) `group <letter> (arms <indices>): <pretty-NF>` line per class. The `<name>` in equiv lines is wrapped in backticks; module paths are not.
5. If the source tree contains `.kio` source but zero `equiv` items, print `no equiv blocks found` (no indent) and exit `0` — absence of tests is not a failure. (A directory with no `.kio` files at all is the CLI-usage error described above, not this case.)
6. Otherwise print a final summary line preceded by a blank line: `result: <n>/<total> equiv blocks passed` on success or `result: <n>/<total> equiv blocks failed` on failure. The noun agrees with `<n>`: `block` for `1`, `blocks` otherwise.

**Failure modes.** Apart from `0` (every equiv passed), `kio test` exits with one of:

- `50` — at least one equiv split into ≥2 normal-form groups (the `5x` test-error tier).
- `16` — an evaluated structural-recursion entry or callback edge failed its totality check, including one executed by an α/η closure-comparison probe.
- `2` — CLI usage error: an unknown flag, an unknown selector, or no `.kio` files in the current directory on the no-selector path.
- Any compile-time category code if step 1 fails before the runner sees an `equiv`.

The full table of category codes lives in [`exit-codes.md`](exit-codes.md).

This is the `kio test` surface.

## `kio fmt`

Formats Kio-family source files in place to the canonical style. The canonical style is specified in [`style.md`](style.md); the rules below describe what the CLI does, not what the output looks like.

Kio's formatting is opinionated and non-configurable: there is one canonical style, and `kio fmt` produces it. Running `kio fmt` on already-formatted source is a no-op.

**Selecting what to format.** With no positional arguments, `kio fmt` walks the current directory recursively and formats every Kio-family source file it finds, skipping build-artifact (`out`, `target`) and hidden (`.*`) directories. With one or more positional arguments, each must be a Kio-family source file or a directory; the formatter visits each in argv order, recursing into directories. Ordinary modules are parsed from their written import clauses, including complete operator grammars, without reading provider declarations. Each selected file retains its declared module-path context: the declaration plus `.kio` must be an exact suffix of the selected path, and a mismatch is a parse error. A nearby `*.pkg.kio` file does not alter that context. Provider availability, visibility, and matching exports are resolution checks, not formatting prerequisites.

Kio-family source files include regular modules (`*.kio`), package files (`*.pkg.kio`), signature changelogs (`*.sig.kio`), dependency declarations (`*.dep.kio`), and dependency lock files (`*.lock.kio`).

**Modes:**

- **Default (rewrite).** Format each visited file in place. Each file whose canonical form differs from disk is listed on stdout, one path per line. A run with no changes prints nothing. Writes are atomic — the canonical form is written to a sibling temp file and renamed over the original — so an interrupted `kio fmt` never leaves a half-written file.
- **`--check`.** Don't write anything. List paths whose canonical form differs from disk on stdout, one per line, and exit `60` if any do (`0` if every file is already canonical). The dirty-found code differs from `gofmt -l` / `cargo fmt --check` (both `1`) because Kio's exit-code table reserves `1` for the internal-error category — see [`exit-codes.md`](exit-codes.md). A CI script ported from either tool needs a one-line substitution. Pairs naturally with positional path arguments.
- **`-` (stdin).** When the sole argument is `-`, read source from stdin and write canonical form to stdout. The stream is parsed as a regular `*.kio` module (there is no way for stdin to identify itself as another Kio-family file kind). Its written import grammars are parsed independently of any provider source tree. Useful for editor integration. Cannot be mixed with file or directory arguments.

**Encoding and line endings.** Source must be valid UTF-8; non-UTF-8 input is rejected at the parse-error category (exit code `11` — see [`exit-codes.md`](exit-codes.md)). Output is always LF; a CRLF or mixed-line-ending input rewrites to LF and is reported as "would change" under `--check` even when no other reformatting is needed.

**Parse failures.** Bail with the parse error's exit-code category — see [`exit-codes.md`](exit-codes.md). The formatter does not attempt partial formatting on syntactically invalid input.

## `kio doc <subcommand>`

`kio doc` is a subcommand container for the package's Kiodoc
content. It has three subcommands — `check` (validate), `fmt`
(canonicalize Markdown snippets), and `build` (validate, then
render a documentation site). All three read the `docs` field of
the `build { ... }` block in the package's
`<name>.pkg.kio` (see
[`package.md` § Build target files](package.md#build-target-files))
and operate on the package rooted at the current directory.
Running outside a package directory, or in a package whose build
block declares no `docs` field, is an error. See
[`kiodoc.md`](kiodoc.md) for the directive contract and the
rendered-output structure.

### `kio doc check [<path>...]`

Validates the Kiodoc directives, embedded Kio snippets, intra-doc
links, and `///` doc-comments in the package, without rendering.

With no positional argument, every markdown file under `docs.md` and every regular module file's doc-comments are validated. With one or more `<path>` arguments, only the matching files are validated — each `<path>` must point at a markdown file under the docs tree or a regular module file under the package. Unknown paths exit at the CLI-usage tier (code `2`) with a diagnostic naming the available files.

`kio doc check` accepts **paths only** (not module names): the two surfaces it validates — markdown files and `.kio` doc-comments — are file-keyed, not module-keyed, and a single dispatch surface keeps the contract obvious. (`kio test`, where the natural unit *is* a module, also accepts module-name selectors.)

**What it validates.** `kio doc check`:

1. Walks the `docs.md` directory recursively, validating every
   `.md` file. For each: parse Markdown fences (visible
   ` ```LANG {attrs}\n…\n``` ` and the hidden HTML-comment form
   `<!--LANG {attrs}\n<body>\n-->`), build the document model
   (harness declarations indexed by name — forward references are
   an error; document-scoped support files; snippets in source
   order; output fences paired with their preceding snippet via the
   pairing rule), and for each `kio` fence not marked `{ignore}`
   validate the resulting Kio input: standalone module snippets,
   harness-substituted snippets, file-backed harness snippets, and
   `variant=KIND` file-kind snippets. The actual check or parser
   exit must match `check_exit_code=N` (default `0`).
2. Walks the package's regular module `*.kio` source files
   (excluding `*.pkg.kio`, `*.sig.kio`, `*.dep.kio`, and
   `*.lock.kio`), collecting
   every `///` doc-comment on the module node and each top-level
   declaration. Each `kio` fence marked `{@}` is wrapped in a
   synthetic top-level fn in the surrounding module's namespace
   and validated with `kio check`. See [`kiodoc.md`](kiodoc.md)
   § Doc-comment input surface.
3. Validates every `` [`name`] `` intra-doc link and every
   `` [`@signature term`] `` / `` [`@source term`] `` /
   `` [`@type term`] `` directive in the prose of both surfaces.

The walker skips build-artifact (`out`, `target`) and hidden
(`.*`) directories. The run-trigger attributes (`stdout`,
`stderr`, `run_exit_code=N`) are recognized only structurally —
their pairing rules are enforced, but the snippet is not run and
output is not compared.

### `kio doc fmt [--check] [<path>...]`

Formats formattable Kiodoc `kio` snippet fences in Markdown files
under the package's `docs.md` tree. It does not format `///`
doc-comments; those are module-source input, not Markdown files.

With no positional argument, every markdown file under `docs.md`
is visited. With one or more `<path>` arguments, each path must
point at a markdown file or directory under the configured docs
tree; directories are walked recursively. Unknown paths, paths
outside the docs tree, and non-Markdown files exit at the CLI-usage
tier (code `2`).

`kio doc fmt` uses the Kiodoc document model, not a raw Markdown
fence scan. For each non-ignored `kio` snippet, it applies the same
snippet placeholders, harness substitution, accumulating-harness
aggregation, file-backed harness assembly, and `variant=KIND`
routing that `kio doc check` uses. The resulting virtual Kio source
is formatted with the same parser and pretty-printer used by
`kio fmt`, and the command rewrites the original fence body only
when the formatted virtual source maps cleanly back to that body.

Opaque/output/text fences are skipped. Invalid snippets and snippets
whose formatted virtual source cannot be mapped cleanly back to the
visible fence body are reported as Kiodoc contract diagnostics
(exit `70`); the command does not partially rewrite a file in that
case.

Modes:

- **Default (rewrite).** Format each visited Markdown file in place.
  Each file whose canonical form differs from disk is listed on
  stdout, one path per line. A run with no changes prints nothing.
- **`--check`.** Don't write anything. List Markdown files whose
  formattable snippet bodies differ from canonical output on stdout,
  one per line, and exit `60` if any do (`0` if every file is
  already canonical).

### `kio doc build [--md] [--html]`

Validates the package (running `kio doc check` first; on a check
failure, aborts with the check's exit code — rendering happens
only against valid input), then renders a per-module documentation
site.

`kio doc build` deliberately takes **no `<path>` filter**: the rendered site's intra-doc links require a whole-package walk, so a "partial site" would either fabricate broken cross-references or silently drop them. The site is whole-package by design — for partial validation during edit cycles, reach for [`kio doc check`](#kio-doc-check-path).

- `--html` renders an HTML site to the build block's `docs.html`
  directory (default `out/docs/`). This is the default format
  when no format flag is given.
- `--md` renders a Markdown site to the build block's `docs.md_out`
  directory (default `out/docs-md/`).

Both flags may be passed in one invocation; both formats are then
rendered. The rendered site's structure, URL scheme, and anchor
format are specified in [`kiodoc.md`](kiodoc.md) § Rendered output.

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — validation passed, and (for `build`) rendering succeeded.
- `70` — at least one Kiodoc contract violation (undeclared
  harness, multi-`@NAME` on one fence, forward harness reference,
  duplicate harness declaration, repeated attribute, unknown
  attribute, missing or orphan output fence, intervening fence
  between snippet and output, attribute-less ` ```kio ` fence,
  `{@NAME}` / `{}` / attribute-less ` ```kio ` inside a `///`
  doc-comment, unresolved intra-doc link or directive term,
  snippet whose `kio check` exit didn't match `check_exit_code`,
  or `kio doc fmt` found an invalid or unmappable Markdown snippet).
- `60` — `kio doc fmt --check` found at least one Markdown file
  whose formattable snippets are not canonical.
- `40` — build error: the package has no package file, or its
  build block declares no `docs` field, or rendering failed.
- `2` — CLI usage error (an unknown flag or subcommand).

**Diagnostics map back to source.** When a snippet's `kio check`
fails, the diagnostic locates the error in the originating source
file — `<path>:<line>:<col>: <message>` referring to the source
line, not a temporary path. On any check failure, the runner also
prints the assembled source for the failing snippet (after harness
substitution, or with the synthetic fn appended for `{@}`
snippets) so the author can see what was actually compiled.

## `kio sig <subcommand>`

Records and gates a package's versioned **contract-surface changelog** (`<pkg>.sig.kio`). The full contract — the compatibility relation, the changelog grammar, and the per-subcommand semantics — is specified in [`versioning.md`](versioning.md); this section is the command-line surface.

Bare `kio sig` is **non-mutating**: it prints the status summary for the package (the same compatibility state `kio sig status` gates on). On a package with no `<pkg>.sig.kio` yet it prints a discoverability line pointing at `kio sig commit` to seal `v(1)`.

**Subcommands:**

- `kio sig stage [--force]` — record the live delta into the current draft. Bare `stage` records a **compatible** delta (writing into the draft's `nonbreaking` partition) and errors if the live surface breaks the last sealed contract; `--force` records a break (writing a `breaking` section), staying unsealed-pending until a `commit`.
- `kio sig commit [-m | --message "<message>"]` — seal the current draft and increment `v(N)`. Records no contract change of its own; errors on any unrecorded delta (compatible or breaking). The optional `-m` / `--message` message is stored as the sealed version block's doc comment and shown by `kio sig log`.
- `kio sig uncommit` — pop the most recent sealed version back into the draft (tip-only). `--force`-gated: it refuses by default with a note that it rewrites a sealed contract (prefer a forward fix), proceeding only with `--force`.
- `kio sig status` — the authoritative CI gate. Exits `0` (clean), `80` (unrecorded break of the sealed contract), `81` (unrecorded compatible drift), or `82` (a recorded break not yet sealed), per the precedence in [`versioning.md` § The `kio sig` command](versioning.md#the-kio-sig-command) and [`exit-codes.md`](exit-codes.md).
- `kio sig log [--breaking] [--since <N>]` — pretty-print the changelog, each version with its commit message. `--breaking` and `--since <N>` are **read-only display filters**.
- `kio sig compact <version>` — collapse the operation-additive history before `<version>` into one synthesized boundary block. The cut cannot exceed the current header generation, so the open draft remains in the suffix. A prefix containing `modify`, `remove`, or a same-name re-add is rejected; under canonical changelog emission, blocks from `<version>` onward retain their original operations and generations. The exact rendered post-compaction changelog reparses and preserves both the complete replay state and the last sealed baseline, so this is log compaction rather than a lossy flatten.

The write-path subcommands (`stage`, `commit`, `uncommit`, `compact`) accept `--stdout` to print the would-be changelog instead of writing the file.

**Package scope.** Like `kio build`, `kio sig` operates per package and uses the same positional disambiguation: an argument that names an existing path (a directory or a `*.pkg.kio` file) is a **package selector**, anything else is not a valid `kio sig` positional. With no selector it discovers and processes every package in the current directory's subtree, each with its own changelog and generation (no cross-package rollback); with selectors it scopes a subset.

**Build-time staleness warning.** `kio check` and `kio build` emit an advisory warning when a package ships a `<pkg>.sig.kio` **and** the live source carries an unrecorded **breaking** delta. The warning is non-load-bearing — it never changes the build's exit code or output (a package without a sig builds identically). A nonbreaking drift is silent; a sig-less package gets no warning. `kio sig status` remains the exit-coded gate. See [`versioning.md` § Build-time staleness warning](versioning.md#build-time-staleness-warning).

## `kio lsp`

Runs the Kio language server. The server speaks the Language Server Protocol (LSP) as JSON-RPC over stdio (the LSP default that every client supports out of the box); editor integrations spawn it as a subprocess and route LSP messages through its stdin/stdout.

`kio lsp` takes no positional arguments and no flags (only `-h` / `--help`).

**Current surface.** Live diagnostics for type / parse / use / name-resolution / elaborator / totality / bridge / dependency errors as the user types, plus LSP-only unused-binding warnings on clean typed snapshots. Hover, goto-definition, find-references, document highlights, document symbols, folding ranges, identifier completion, whole-file formatting, per-token semantic classification, rename, inlay hints, signature help, and quick-fix code actions are implemented. The advertised `serverCapabilities` track what the server actually implements — clients that look for unimplemented capabilities will see them absent and won't issue the requests.

Concretely, the server advertises:

- `textDocumentSync` with `openClose = true`, `change = Incremental`, `save = { includeText: false }`. Incremental sync means the editor sends `textDocument/didChange` with range-keyed edits; the server applies them to its in-memory overlay and reanalyzes the package after a brief debounce. The overlay is authoritative for as long as a document is open — analysis reads from it, not from disk, so out-of-band disk edits don't affect diagnostics until the editor sends a matching `didChange` or the document is closed and reopened.
- `hoverProvider = true`.
- `definitionProvider = true`.
- `referencesProvider = true`.
- `documentHighlightProvider = true`.
- `documentSymbolProvider = true`.
- `foldingRangeProvider = true`.
- `completionProvider = { triggerCharacters: [":", "[", "(", ","], resolveProvider: false }`.
- `documentFormattingProvider = true`.
- `semanticTokensProvider = { legend: { tokenTypes: […], tokenModifiers: […] }, full: true, range: false }`. The legend and token/modifier encoding are documented in the `textDocument/semanticTokens/full` entry below.
- `renameProvider = { prepareProvider: true }`. The server supports both `textDocument/prepareRename` and `textDocument/rename`; see entries below.
- `inlayHintProvider = { resolveProvider: false }`. The server supports `textDocument/inlayHint`; see entry below.
- `signatureHelpProvider = { triggerCharacters: ["(", ","] }`. The server supports `textDocument/signatureHelp`; see entry below.
- `codeActionProvider = { codeActionKinds: ["quickfix"], resolveProvider: true }`. The server supports eager quick fixes for diagnostics that carry materialized edits and lazy quick fixes resolved through `codeAction/resolve`.

And handles:

- `initialize` / `initialized` — handshake.
- `textDocument/didOpen` — seed the overlay with the editor's text and version, schedule reanalysis.
- `textDocument/didChange` — apply each `TextDocumentContentChangeEvent` to the overlay in order, bump the stored version, schedule debounced reanalysis. Successive edits within the debounce window coalesce to one analysis.
- `textDocument/didSave` — informational (the overlay is already authoritative). Schedules reanalysis so editors that auto-save without sending intermediate `didChange` notifications still see fresh diagnostics.
- `textDocument/didClose` — drop the overlay entry; subsequent reads fall back to disk. Diagnostics for closed files stay published until the editor clears them (LSP convention).
- `shutdown` / `exit` — clean shutdown.

Typed foreground requests (`hover`, `definition`, `references`, completion detail, prepare-rename, and rename) do not wait for the normal diagnostics debounce when the stored typed snapshot is missing or stale for the open document version. They schedule an immediate analysis request, still return from the latest available snapshot for that JSON-RPC response, and let the fresh result arrive asynchronously through later diagnostics / typed requests. Hover, definition, and completion detail may satisfy that fresh request with a focused single-module typed shard for the file under the cursor; references, prepare-rename, and rename require full-package typed analysis because their answers are workspace-wide.

Syntax-backed requests authenticate a concrete `file:` URI and the exact file-path/declaration correspondence before using a parsed or lazy module. Relative or otherwise invalid file identities and path/declaration mismatches are not reinterpreted through the workspace: hover, completion, and signature help return `null`, while code actions retain only self-contained eager fixes from `Diagnostic.data.fixes`. Once that file context is authenticated, completion, signature help, and contextual code actions may retain their lazy-header behavior when a body is incomplete. Hover requires a complete applicable parse of the current document; if that is unavailable, it returns `null` instead of consulting stale typed facts. Closed files enter the same file-context parser directly rather than falling back to context-free parsing. Untitled and other non-file buffers parse their own written import grammars without a provider source root. Provider changes may affect typed semantic answers and import-list suggestions, but never supply the consumer's parse grammar.

Block-elaborator heads navigate to their ordinarily selected declarations. Continuation labels support hover, definition, references, document highlights, prepare-rename, and rename. A label is identified by its selected elaborator declaration and descriptor position; equal spellings in another elaborator or an ordinary value binding are separate. Hover shows the block exposure and declaring elaborator. Rename changes the descriptor label and its corresponding call-site labels, rejecting a collision with another label of the same elaborator. These label queries require the captured provider sources to remain current; changing only a provider cannot authorize navigation or edits from an older label snapshot. Parsing, formatting, and source token classification remain independent of provider definitions.

Block-call completion uses the current selected declaration header. A snippet-capable client receives an editable prefix-values region and the ordered trailing-block scaffolds; the prefix region does not assert an argument count. Continuation completion offers only the next declared label after the already-written matching labels, not labels from unrelated elaborators. Block signature help displays the public call type, a prefix-values region, and each block's exposure and label, highlighting the current region even in an incomplete body. Prefix arguments follow ordinary public call semantics; descriptor count and private implementation grouping do not determine their arity.

A wrong continuation label can offer a token-local replacement which is fully rechecked before publication. A missing trailing block can instead offer “Add missing trailing block scaffolds.” This explicitly maybe-incorrect action inserts labelled empty bodies for the user to fill; it does not claim to repair type errors or invent a sequence result. Its exact compiler-produced edit is regenerated against the current source and providers, and its resulting syntax is checked. Ordinary eager fixes retain their full semantic recheck.

- `textDocument/hover` — returns Markdown content for the token under the cursor, plus a range highlighting that token or expression. Declaration hovers show the declaration signature and its rendered `///` Kiodoc prose. A recursive type-group member shows its complete written `rec { ... }` context with only that member's prose; the group's `rec` keyword shows the complete declaration and its separate outer prose. Recursive-label nominal targets retain their owning labels declaration and recursive context. These source views follow [Kiodoc's group-aware rendering](kiodoc.md#what-signature-renders). User-defined elaborator declarations in the current file, `__intrinsics__`, and `__comptime__` names show their builtin documentation. Expression hovers fall back to the synthesized type as a fenced code block `` ```kio … ``` ``. Returns `null` when no hoverable declaration, builtin, or typeable expression is under the cursor or the URI-backed parser context is unavailable.
- `textDocument/definition` — returns a `Location` pointing to the declaration site of the binding under the cursor. Top-level functions, newtypes, host functions, locals, and type parameters resolve to the file and span that declares the item. Intrinsics and import aliases have no source-file jump target in this response and return `null`.
- `textDocument/references` — returns the list of all use-sites for the binding under the cursor, across the entire package. The response list is sorted by file URI then by position. `includeDeclaration` is honored: when `false`, the binding's semantic declaration span is excluded regardless of whether the request cursor is on that declaration or on a use; the queried use remains in the result. Returns `null` when no resolvable binding is under the cursor, `[]` when a binding is found but no reference spans were recorded.
- `textDocument/documentHighlight` — returns the occurrences of the binding under the cursor within the current file only, as a `DocumentHighlight[]` sorted by position with `kind: Text` on every entry (the analysis does not distinguish read from write sites). This is the single-file restriction of `references`: it never crosses file boundaries, so it answers from a stored snapshot without waiting for full-package analysis. Returns `null` when no resolvable binding is under the cursor.
- `textDocument/documentSymbol` — returns a flat `DocumentSymbol[]` list in source order. Ordinary top-level items contribute one entry; a recursive function or type group contributes one ordinary entry per member in member order. A type group is never one opaque symbol that hides its members. Each entry carries `name`, `kind` (see SymbolKind mapping below), `range` (the full item or recursive-member span), `selectionRange` (the name identifier's span), and no children. The source text is read directly from the overlay and parsed through the lazy header/body-thunk parser, so the response is available even when a previous typecheck failed or a function body is syntactically broken. Returns `null` when the source is not available or the declaration/header surface itself does not parse.
- `textDocument/foldingRange` — returns one `FoldingRange` per multi-line `{ … }` brace group, with `kind: "region"`. Single-line brace groups are omitted. Recovered (unbalanced) groups still produce a range using the skeleton-synthesized close position. The response derives from the CST tree-skeleton and does not require a successful typecheck. Returns `null` when the source is not available.
- `textDocument/completion` — in an ordinary lexical context, returns a `CompletionList` with `isIncomplete: false` containing every identifier that is lexically in scope at the cursor position. Each item carries `label` (the offered spelling), `kind` (`Function` / `Variable` / `Module` / `Keyword` / type-shaped kinds), `detail` (the synthesized type as a Kio type string, when a current analysis identifies that exact lexical declaration), and Markdown documentation when a local `///` doc-comment or builtin surface doc is available. Shadowing is respected: when an inner binding shadows an outer one, only the inner binding appears. Optional detail and documentation are omitted when the selected declaration's identity or source version cannot be authenticated; the same spelling elsewhere does not supply metadata. Completed earlier module-level function and elaborator declarations, explicitly imported names in the requested namespace, and `import … as alias` qualified-import aliases are included. A nonrecursive function is absent from its own body, and current recursion-group members are not ordinary values. Fn value parameters, destructuring-pattern names, source-ordered neutral-block bindings, `let`-bound locals, and row-let payload locals are included when the cursor is inside the enclosing scope. In type positions such as after `:`, completion filters to type-shaped names (`_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`): aliases, newtypes, imported type names, reflected types, and type parameters. Inside `rec newtype` it includes that singleton head; inside `rec labels` it includes the declaration's generated heads and named alias head; inside a bare type `rec` group it includes every member head, including later members. The same self/later heads are absent from an unmarked declaration or ordinary source-ordered position, and binder shadowing still wins. These legal recursive heads come from the parsed scope graph even before a typed snapshot exists. Inside a selective import list, completion instead offers declarations visible to the importer from the explicitly selected provider's current declaration snapshot, filtered by the selection namespace. Operator entries insert the complete tagged grammar; import-list entries carry their declaration signature and available provider documentation. A current declaration snapshot does not require a successful typecheck. The list is incomplete when a required provider snapshot or selected identity edge is unavailable. This semantic query does not supply the consumer's parse grammar. Returns `null` when the source is unavailable or a concrete file's URI/declaration identity is invalid.

  Closed contextual positions offer exactly the language-defined named choices that can still complete an accepted construct, across module, package, dependency, lock and signature files. Reserved, removed, duplicate, mutually exclusive and order-invalid choices are excluded. Comments, literals and declaration-name positions do not receive unrelated identifier suggestions. Grammar context selects the namespace and eligible operator spellings; language-name matching respects the type/value naming namespace. Qualified paths follow the explicitly selected module or nominal identity, not unrelated providers. The replacement range covers the complete started identifier or operator run. Responses retain every eligible match without a result-count cap.
- `textDocument/formatting` — formats the entire document to the canonical style produced by `kio fmt`. The target document comes from the editor's in-memory overlay when open, otherwise from the latest analysis snapshot. A concrete absolute `file:` URI authenticates its decoded lexical path against the document's declared module path, never through workspace/package discovery. The document's written import grammars are parsed without reading provider modules or their overlays; non-file buffers likewise need no provider source context. Parse errors remain silent no-ops because the diagnostics channel already reports them. A relative, query-bearing, fragment-bearing, or otherwise invalid `file:` URI is not reinterpreted as an untitled workspace buffer and returns no formatting result. Returns a list of zero or one `TextEdit`s: an empty list when the text is already canonical or an ordinary parse error prevents formatting; one full-file replacement edit otherwise. The Kio-family file kind (`*.kio`, `*.pkg.kio`, `*.sig.kio`, `*.dep.kio`, or `*.lock.kio`) is inferred from the URI's filename; `untitled:` and other scheme-only URIs fall back to plain `.kio` treatment. Formatting options (`tabSize`, `insertSpaces`, etc.) are accepted but ignored — Kio's canonical style is non-configurable.
- `textDocument/semanticTokens/full` — returns a `SemanticTokens` object with a delta-encoded token array classifying every token in the document. Reads the editor overlay (if the document is open) or the latest analysis snapshot. Concrete absolute `file:` URIs retain the same path/declaration validation as other syntax-backed requests. AST-derived classification parses the document's written operator grammars without consulting provider sources or overlays, including for untitled buffers. A syntactically incomplete buffer retains lexical classification and roles established by successfully parsed source prefixes; an invalid `file:` identity returns `null` rather than being reinterpreted through the workspace. Returns `null` when no source text is available. Token classification is lexer/parser-derived: keywords, comments, strings, numbers, operators, function definition names, elaborator call heads, type names (including the optional leading underscore), module path segments, label names in construction/access/update/row-let entries, and parameter or row-let binders are each classified from the source token stream. The `data` field is a flat array of `u32`s, five per token (`deltaLine, deltaStart, length, tokenType, tokenModifiers`), relative to the start of the previous token. Token type indices are fixed by the legend advertised at `initialize`; see the table below.
- `textDocument/prepareRename` — returns a `RangeWithPlaceholder` (the identifier span at the cursor and its text as the placeholder) when the cursor is on a renameable identifier. Row-let aliases are renameable local declarations; a shorthand row-let entry such as `let .({abc})` prepares the local span `abc`. Returns `null` when the position is on whitespace, a literal, an intrinsic, a `host fn`, a qualified import alias, a bridge glob, or any other non-renameable position — the editor disables the rename popup for those positions. For a placeholder family, the intro stem or an owned numbered reference prepares only the stem portion (`x` in `.x.` or `x2`). Requires a recent analysis snapshot; returns `null` when none is available.
- `textDocument/rename` — renames the identifier at the cursor to `newName` across the entire package. Returns a `WorkspaceEdit` on success. For editor-facing requests, the edit uses versioned `documentChanges` so open-document edits are tied to the analysis snapshot's document versions; clients can reject the edit if the buffer has moved on. Renaming an ordinary newtype includes its written type occurrences, including explicit call type arguments; same-spelled lexical type parameters retain their separate identities. Row-let rename preserves selected labels: renaming shorthand `let .({abc})` to `payload` rewrites the declaration to `let .({abc as payload})` and updates local references; renaming an explicit alias changes only the alias. A placeholder-family rename changes the intro stem and exactly its owned references, preserving each numeric index: renaming `x` to `arg` changes `.x.` / `x2` to `.arg.` / `arg2`. Nested owners, shadowed references, and qualified paths stay unchanged. The new stem must be valid and end in a letter; a rename that changes ownership or arity is refused. Returns a JSON-RPC error (`RequestFailed`, code `−32803`) when: (1) the cursor is not on a renameable identifier; (2) `newName` is not a valid spelling for the resolved binder's namespace — type binders require `_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`, value binders require `_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*`, and reserved or letterless spellings remain invalid; (3) `newName` would shadow or collide with an existing binding at any reference site (conservative conflict check — refuses rather than silently altering semantics). Explicit newtype constructors and projectors are checked against the other member of their exact owning newtype; members of unrelated newtypes do not enter the module's binding namespace. **Scope:** cross-file within the current package; package-external rename is not attempted. `pub op` bindings are refused because renaming a `pub op` affects parser behavior in all importers. Doc comments that mention the old name by hand are not rewritten — the user fixes those manually. Returns `null` when no analysis snapshot is available.
- `textDocument/inlayHint` — returns range-filtered `InlayHint[]` entries from the latest typed snapshot. Unannotated `let` binders get a type hint at the binder-name end (`: <type>` rendered with the compiler's `write_type` renderer). Calls with inferred or value-reclassified type arguments get a type-argument hint at the callee end (`[T0, T1, …]`, also rendered with `write_type`). The provider does not resolve hints lazily; `resolveProvider` is `false`. Returns `null` when no analysis snapshot or source text is available.
- `textDocument/signatureHelp` — when the cursor is inside a direct-path call, returns a `SignatureHelp` whose signature label is rendered from the resolved top-level Kio `fn` or host `fn` declaration. Ordinary calls highlight value arguments only: written or inferred type-argument slots do not increment `activeParameter`; a cursor inside a type-argument slot reports the current value slot. Block elaborators use the current-header region display described above. Returns `null` for other unsupported callee shapes (locals, newtype constructors, operators, blockless user elaborators, complex callee expressions), when the required current metadata/source is unavailable, or when a concrete file's syntax context is invalid.
- `textDocument/codeAction` — returns quick-fix `CodeAction`s for diagnostics. Eager actions materialize directly from `Diagnostic.data.fixes` and carry their `WorkspaceEdit` in the response, including when a concrete file URI or path/declaration context is invalid because those edits are already self-contained in the diagnostic. Recursive-data fixes are compiler-produced from the resolved declaration/SCC graph, never inferred from diagnostic prose. A provably valid missing singleton offers exactly “Add `rec` to this recursive newtype” or “Add `rec` to this recursive labels declaration”; a redundant singleton offers “Remove unnecessary `rec`”; and a safely transformable mutual or malformed group offers “Fix recursive type groups.” A fix preserves declaration text, docs, comments, visibility, and stable order, is reparsed and rechecked before publication, and is withheld during recovery, overlapping/ambiguous edits, unsafe moves, or any case where it cannot prove a valid result. The group action may wrap one genuine missing SCC, split independent SCCs, unwrap/topologically order an acyclic group, move acyclic helpers outside, or rewrite a singleton component to its modifier form. It never invents `rec type` or nominally wraps an alias-only cycle. Lazy actions carry enough `CodeAction.data` to be resolved by `codeAction/resolve`; these include auto-imports for unresolved names exported by another module in the current package and add-stub actions for unresolved names exported by no module. Context-dependent lazy actions are omitted when the file syntax context cannot be authenticated. Auto-import is a client-driven edit oracle, not a resolution rule: accepting the action inserts an explicit import in the current file (qualified `import path as alias;` plus a use-site rewrite, or selective `import path(name);` when the alias is already bound). Adding declarations elsewhere may change the suggestion list, but it never changes how existing source resolves unless the user applies the edit.
- `codeAction/resolve` — fills in the edit for lazy quick fixes. Auto-import resolution emits the import edit and, when using a qualified alias, the use-site rewrite. Add-stub resolution appends a top-level stub `fn name(arg0: ., ...) -> . { () }` at end of file. Add-stub is deliberately maybe-incorrect because placeholder types and the stub body require user repair.

  **Token-type legend.** The legend positions (by index) are:

  | Index | Type name        | Kio tokens classified here                                        |
  |-------|------------------|-------------------------------------------------------------------|
  |  0    | `namespace`      | Module path segments (`module a/b;`, `import a/b(…);`)          |
  |  1    | `type`           | Type-definition names (`newtype Box`, `type List`)               |
  |  2    | `typeParameter`  | Reserved for type-parameter binders (currently mapped to `parameter`) |
  |  3    | `parameter`      | Value-parameter binders (`x` in `fn f(x: T)`) and type-parameter binders (`[A]`) |
  |  4    | `variable`       | Plain identifier references (uses of let-bound and other names)   |
  |  5    | `function`       | Function definition names, call-site callee names, and elaborator heads |
  |  6    | `enumMember`     | Label entry names (`foo:` inside `labels { foo: T, … }`)            |
  |  7    | `keyword`        | Keywords (`fn`, `let`, `module`, …) and boolean literals  |
  |  8    | `comment`        | Line comments                                                     |
  |  9    | `string`         | String literals                                                   |
  | 10    | `number`         | Numeric literals                                                  |
  | 11    | `operator`       | Operators (both builtin and user-defined)                         |

  **Token-modifier legend.** The modifier bit positions are:

  | Bit | Modifier name    | Applied to                                                        |
  |-----|------------------|-------------------------------------------------------------------|
  | 0   | `declaration`    | The definition site of a binding (fn names, type names, label names, parameter binders) |
  | 1   | `defaultLibrary` | Reserved for host-provided items                                  |
  | 2   | `readonly`       | All Kio binders (every binding in Kio is immutable; applied to function names, type names, and parameter binders) |

  Structural punctuation (`( ) { } , ;` and parser-confirmed forall `[` / `]`) and slot tokens (`_`) are covered by the grammar-level highlighter rather than emitted as semantic tokens. Placeholder-lambda stems and numbered references use the ordinary parameter and identifier categories; `#` is an ordinary operator character. Contextual keyword roles come from the parser rather than a flat word list. A line-local regex highlighter may leave a genuinely later-line-dependent role unclassified; compiler-derived highlighting uses the complete source context.

**SymbolKind mapping.** The mapping from Kio item variants to LSP `SymbolKind` values is:

| Kio item  | LSP `SymbolKind`  | Rationale                                                 |
|-----------|-------------------|-----------------------------------------------------------|
| `fn`      | `Function` (12)   | Direct equivalent.                                        |
| `type`    | `Interface` (11)  | A type alias is an abstract shape, not a value.           |
| `literal` | `Constant` (14)   | A named literal is value-like at use sites.               |
| `newtype` | `Struct` (23)     | A nominal wrapper with constructor and projector.         |
| `labels`    | `Enum` (10)       | A family of named injections, analogous to an enum.       |
| `equiv`   | `Operator` (25)   | An equivalence claim; no closer LSP analogue.             |
| `elaborator` | `Function` (12) | It introduces a callable bang form.                         |
| `op`      | `Operator` (25)   | Direct equivalent.                                        |

Anonymous `labels { … };` blocks (no declared name) are omitted from the symbol list.

Other requests return an LSP `MethodNotFound` error so the client can recover.

**Package discovery** for typed analysis mirrors `kio check`: the file's package root is the nearest ancestor directory containing a `*.pkg.kio`. Files outside any package fall back to single-file analysis with no ordinary `import` resolution. This analysis membership does not define the parser source root used by file-backed formatting, hover, or semantic classification; those syntax boundaries use the file-path/declaration rule above.

**Diagnostic mapping.** Each `Error::{Parse, Use, NameRes, Type, Elaborator, Totality, Bridge, Dep}` becomes one LSP `Diagnostic` with `severity = Error`, `source = "kio"`, the original error message, the source span converted to an LSP `Range`, and `code` set to the numeric exit-code category for that error (`11` parse, `12` use, `13` name resolution, `14` type, `15` elaborator, `16` totality, `20` bridge, `30` dependency). Secondary labels become standard `relatedInformation`: a same-file label uses the primary URI and line index, while a file-qualified label uses the exact related file's URI and source index. If that exact source, URI, or range is unavailable, only the related entry is omitted; the span is never clamped into or reassigned to the primary document. This uses the standard LSP field and adds no protocol extension. Diagnostic help and notes are preserved in `data`; structured fixes are preserved as `data.fixes[]`, each with `title`, `applicability` (`machineApplicable` or `maybeIncorrect`), and same-file text edits. A compiler-validated recursive-scope fix also carries `followOnReanalysisOutside`, the exact source range whose semantics the edit may change. Its action remains available when rechecking exposes a different first error only if that error's unchanged source span maps wholly outside this range. Unresolved-name diagnostics additionally carry `data.unresolvedName` for lazy code actions. An analysis pass may publish multiple independent diagnostics from a coherent analysis snapshot. Diagnostics that merely cascade from a failed prerequisite are suppressed; no cross-category selection order is promised when independent errors coexist. On a clean full-package typed snapshot, unused `let` and value-parameter binders that do not start with `_` publish LSP-only `Warning` diagnostics with underscore-prefix fixes, and local `let` warnings may also carry a maybe-incorrect remove-let fix. URIs that previously had a diagnostic but no longer do get an empty `Vec<Diagnostic>` to clear them (LSP replaces published diagnostics wholesale per URI). Each `publishDiagnostics` notification's `version` field carries the overlay version the analysis ran against, so clients can correlate diagnostics with their local document state.

**Recursive-data diagnostics.** A missing singleton marker reports `` recursive data declaration requires `rec` `` on the recursive reference, relates the declaration head, and names the complete required spelling; diagnostic lookup does not bind the invalid reference for navigation or rename. A redundant singleton marker reports `` this `rec` marker is unnecessary `` on the keyword. A later ordinary declaration reports that it is declared later and relates that declaration's head. Bare type-group diagnostics distinguish no cycle, multiple independent components or helpers, one-member/group-modifier misuse, wrong-kind members, and an alias-only component with no nominal boundary. Exact category assignment follows [`exit-codes.md`](exit-codes.md). Recursive-scope compile errors follow the compiler's ordinary deterministic first-error flow: when one document has independent group and singleton defects, applying the currently published repair and reanalysing may expose the next repair. `source.fixAll` combines only the diagnostics in its current coherent snapshot; it does not speculate about errors hidden behind a failed prerequisite. Once a complete edited document forms a valid singleton or group, publication for that exact document version retracts all stale recursive-scope diagnostics.

**Position encoding.** UTF-16 only, the LSP default for `Position.character`.

**Exit codes.**

- `0` — clean shutdown (the client sent `shutdown` then `exit`).
- `1` (internal-error category) — the server hit an unrecoverable error before or during the handshake (cannot read stdin, malformed initial message, etc.). Analysis errors do *not* exit non-zero; they flow through `publishDiagnostics`.
- `2` (usage category) — `kio lsp` was invoked with positional arguments or an unknown flag.

## `kio repl [<selector>...]`

Opens an interactive prompt for inspecting the Kio modules in a directory — query types and purity, read doc-comments, print canonical source, browse, and navigate cross-references. Modules load by their module path (FQN); a `*.pkg.kio` package file is optional and never gates the REPL. Loading or refreshing a module checks the directory snapshot through parsing, operator folding and surface lowering, name resolution, and Lowered typechecking. Bare-expression queries additionally substitute the recorded completions and validate the resulting Prime artifact; normalization uses the checked Lowered expression and the host-independent reduction relation. The REPL never executes the package against a host. Every turn is either a *meta-command* (a line beginning with `:`) or a bare-input query classified as a name, expression, and/or loaded module path; bare compound expressions print their synthesized type and residual normal form — the `:t` and `:normalize` views together.

The prompt is two-line: the **current module's path** renders above the `kio>` input symbol — or `(no module — :load <module-path> to begin)` when no module has been loaded yet — so the user always sees which scope a bare-name query resolves against.

`kio repl` opens on a **blank slate** — with no selectors, no modules are loaded at startup; use `:load <module-path>` to bring a module in. `kio repl` accepts an optional positional `<selector>...` list to load specific modules at startup instead; each selector matches a module by its module path (`op/main`), by the filename of its `.kio` file (with or without the `.kio` extension), or by the file's stem. A selector that matches nothing aborts startup with a usage error.

`kio repl` runs on the module tree rooted at the current directory; a `*.pkg.kio` package file is optional. The directory's `.kio` modules are discovered and loaded by their module path, whether or not a package file is present.

**Input.** Every input is either a *meta-command* (it begins with `:`) or a bare-input query. A `<name>` argument is resolved through the *current scope* (the items the current module declares, the names it imports, the module aliases it sets) or by fully-qualified path (`a/b.name` — module `a/b`, item `name`). A `<module-path>` argument is a loaded module's path, or a module alias the current module binds.

**Prompt grammar — expression vs. name/path.** The expression category uses the same grammar as module code: a value path is a local or imported name, and `m.f` reaches an item through a module alias. A slash-qualified FQN such as `a/b.item` is not a value path; source expressions import the item selectively or import `a/b` under an alias first. In an expression position, `/` has only its explicitly imported operator meaning, so `a / b.item` remains an operator expression when that binding is in scope. The REPL separately recognizes `a/b.item` as a name/FQN argument, and a bare module path as a loaded-module reference. Module references otherwise occur only in path-taking command arguments (`:ls a/b/c`) and `import` clauses.

**Command input categories.** `:t` and `:pure` deliberately accept either a name/FQN or an expression as two distinct categories. A spelling that resolves as a declared name or exact item FQN, or names a loaded module, stays in the name category. Otherwise, a path-shaped bare-name token sequence is tried as an expression in the current module's grammar: this admits an alias-qualified value path such as `value.item`, and admits a slash-operator expression with either a bare or dotted right operand when `/` is in scope, without making an FQN part of the Kio expression grammar. A successful expression parse commits that category, including when typechecking then reports an expression diagnostic; if that parse also fails, the name-resolution diagnostic wins. Other expression shapes are type-checked directly. `:normalize` takes only an expression. `:load`, `:unload`, `:ls`, `:signature`, `:source`, `:doc`, `:which`, and `:refs` take a name or fully-qualified path (a declared entity to inspect).

**Fallible sibling-command correction.** On a mismatch, a single-category command offers the same correction in a fixed order: (1) run the argument in the command's own category; (2) only if that *fails*, and (3) only if the argument *syntactically looks like the other category* — a name-only argument that is a compound expression (`:signature 1 + 2`), or an expression-only argument that is a slash-qualified FQN (`:normalize a/b.item`) — append a one-line suggestion of the sibling view. The shape test is a cheap syntactic guess that only gates the hint: it never overrides a command that succeeds, and the REPL never silently re-dispatches. Because `:t` and `:pure` accept both categories, neither a name/FQN nor an expression is a mode mismatch for them.

An input may span multiple lines. When the submitted text has an unclosed parenthesis, brace, structurally recognizable forall-binder bracket, or an unterminated string literal at its end, the input is *incomplete*: the prompt shows the `...` continuation indicator and gathers further lines until every structural delimiter is closed and every literal terminated, then submits the whole input at once. Square brackets inside ordinary maximal operator runs do not participate in continuation balancing: `a [! b`, `a [ b`, and a bare `[` submit on the first Enter, while an unfinished `.[A` or `.[*F` forall binder continues. Input the lexer rejects outright (a stray byte that is not a token) is treated as complete and submitted, so the parser surfaces the error rather than leaving the user stranded in a continuation. Over a pipe (a fed transcript), the same gathering applies — continuation lines are joined into one input — so a piped transcript and an interactively-typed one normalize identically.

- `:load <module-path>` — load a module by its module path. The module's `import`-clause dependencies that resolve to other modules discovered in the directory tree are loaded alongside it (implicit loading); the whole load commits or fails atomically (a type error anywhere in the analyzed module tree, an unreadable file, or a missing module aborts it with the session unchanged). Re-loading an already-loaded module re-reads its file. The loaded module becomes the *current* module.
- `:signature <name>` — print a named item's declaration header: the declaration line with no body, the same artifact Kiodoc's `` [`@signature term`] `` directive embeds. Resolves any named declaration — `fn`, `op`, `newtype`, `type`, `literal`, `labels`.
- `:source <name>` — print the canonical (`kio fmt`) form of a named item — the declaration with its body — prefixed by the contiguous run of `//` and `///` comment lines immediately above the declaration on disk. The run stops at the first blank line above the comment block (or at the previous declaration's end).
- `:t <name-or-expression>` — print the synthesized type. A name on its own reports a function's type. A name denoting a type rather than a value — `newtype` / `type` / `labels` — is a kind-aware error: a type-level name binds no value (use `:signature` for its declaration header). A `literal` alias has no standalone type; `:t name` tells the user to ask about an annotated expression such as `name(Type)`. A *module path* (`a/b`) is not a value either; the error points at `:ls a/b`, which lists the module's items. A resolvable name, item FQN, or loaded module path uses this name branch. Otherwise, a path-shaped spelling falls through to the expression branch when the current module grammar parses it, including an alias-qualified path such as `value.item` and an imported `/` expression with a bare or dotted right operand; if parsing also fails, `:t` reports the name-resolution failure. Any other expression — a literal, an application, an operator expression — is type-checked and its synthesized type is printed. A compound argument that is neither a name nor a parseable expression gives the same kind-classified invalid-input line a bare line gives, never a raw parser error.
- `:pure <name-or-expression>` — print `pure` or `impure` using the compiler's function-purity contract, without evaluating the value. On the name/FQN branch, an ordinary `pure fn` is pure; an unmarked `fn` or any `host fn` is impure. A declaration that is not an executable value binding receives a kind-aware diagnostic. On the expression branch, `pure` means that the same expression is admitted in an ordinary `pure fn` body; if it is admitted only in an unrestricted function body, the result is `impure`, while an expression rejected in both contexts reports its syntax, type, package, or no-current-module diagnostic. The pure context applies through nested lambda bodies, so merely constructing a lambda whose body refers to an impure value is reported as impure. Resolution and category commitment are identical to `:t`: exact declaration names and FQNs use declaration metadata, while calls and module-alias-qualified value paths use ordinary Kio expression resolution.
- `:normalize <expression>` — print the expression's **residual normal form**: the value left after applying every reduction the host-independent partial evaluator admits — β-reduction, `let`-unfolding, `fn`-call inlining, intrinsic reduction over expanded elaborator code, the same reduction relation `equiv`-discharge uses ([`formal/equiv.md`](formal/equiv.md)). An executed structural-recursion Totality fault is reported in the Totality category rather than printed as a residual normal form. Closed pure expressions collapse to ground values (`()`, `42`, `"hi"`, a closure); host items remain as opaque atoms; applications that can't be reduced print as `callee(arg, …)`. A residual closure prints as an informational marker such as `<closure provider.wrap(x)(y)>`; an anonymous closure also names its source span and, when present, its enclosing item. The marker is not Kio syntax and deliberately omits the body and captured values, so matching closure markers do not establish equivalence. When an inlined dependency leaves a newtype constructor or projector in a non-closure residual that the current module's scope cannot identify unambiguously, the result begins with the minimum ordinary qualified `import <module> as <alias>;` clauses needed to distinguish those exact declarations, followed by a blank line and the residual expression. These clauses provide identity context for the displayed result; they do not mutate the REPL scope or assert that the output is a standalone program that would type-check if pasted elsewhere. In particular, the residual may still refer to private declarations or depend on surrounding type context. No clause is added when the current scope already supplies an unshadowed spelling. Accepts a single name too, forcing reduction where a name typed on its own would otherwise take the kind-aware shortcut below.
- `:doc <name-or-module>` — render a name's `///` doc-comment plus its signature, through the same Kiodoc rewriter `kio doc build` uses (`` [`@signature term`] `` / `` [`@source term`] `` / `` [`@type term`] `` directives expand, `` [`name`] `` references render as code spans). A name with no doc-comment shows the signature alone. When the argument is a loaded module's path, renders the module's summary instead: its leading `///` doc-comment (when present) plus a one-line count of declared items and `import`-clause imports.
- `:ls [-v] [<module-path>]` — list the top-level items of a loaded module. With no argument, lists the current module's items; with no current module, the command prints the "no current module" diagnostic. By default each item renders as its keyword + name (`fn view`); `-v` renders each through its full signature (`fn view[S][A](…) -> A`), the form an `op` already shows. A terse listing that elided a signature appends a dim trailer advertising `-v`.
- `:mods` — list loaded modules in load order. The current module is marked `*`; an implicitly-loaded module shows the explicit module that pulled it in (`x/dep (via x/a/b)`).
- `:packages [<package-name>]` — a read-only view of the directory tree's `*.pkg.kio` package files, the package counterpart to `:mods`. Packages depend on modules, not the reverse, and a package file is optional, so this command never touches the loaded-module set. With no argument, lists every `*.pkg.kio` file under the directory root by its package name (the `.pkg.kio` stem) and the relative directory it lives in. With a package name, parses that package file and renders its contract: the package name and the `bridge { … }` glob list selecting the modules whose `pub` items form the host contract surface.
- `:scope [-v]` — list everything in the current module's scope: declared items (`fn` / `newtype` / `type` / `literal` / `labels` / `op` / `equiv`), names brought in by `import` clauses grouped by source module, module-local operator bindings, module aliases bound by `import … as …;`, and whether `import __intrinsics__;` is in scope. `-v` renders the declared items through their full signatures, the same flag `:ls` honors; a terse listing that elided a signature appends the same `-v` trailer. With no current module, prints the "no current module" diagnostic.
- `:unload <module-path>` — remove a loaded module. Rejected for an implicitly-loaded module (its explicit referent is unloaded instead) and when another loaded explicit module still references it. On success, implicit dependencies that become orphaned are removed too; if the unloaded module was current, the most recently loaded remaining explicit module becomes current.
- `:which <name>` — report which loaded module declares a name, as a fully-qualified path. Includes private (non-`pub`) declarations.
- `:refs <name>` — list every reference to a name across the loaded modules (call sites, type-position uses, `import`-clause names), each as `module-path:line:col  in <kind> <name>` where `<kind> <name>` is the enclosing top-level item's spelling (`in fn foo`, `in newtype T`, `in op _ + _`, `in equiv e`, `in type A`, `in labels T`). A reference outside every item — an `import` clause's name, the `module ...;` header — is labeled `in module <module/path>`.
- `:help` — list the commands.
- `:reset` — drop every loaded module and clear the current module. The on-disk history file is kept.
- `:quit` — leave the REPL. End-of-input (Ctrl-D) is equivalent.

**Bare input.** An input that does not begin with `:` is classified into the kinds it matches — an input can match more than one (a bare identifier is both a *name* and an *expression*): **name** (lexes as an identifier, operator, dotted path, fully-qualified `mod/path.item`, or module path — name-*kind* even if it does not resolve), **expression** (parses as a Kio expression in the current module's scope), and **module path** (a `/`-joined path or a single identifier naming a loaded module). A mixed `mod/path.item` spelling is a name, not a direct expression FQN; it also matches the expression category only when the current module explicitly imports `/` as an operator and the same tokens form a valid operator expression. The classification drives both the default action and a footer:

1. **`:doc`** — the default action for a bare name (a single identifier, a module-qualified item path `a/b.f`, an operator symbol, a parenthesized operator, or a loaded module's path `a/b`). The doc-comment + signature renders, or — for a module — the module summary. Resolution failures inside `:doc` print `:doc`'s own diagnostic.
2. **The expression views** — the default action for input that parses as a Kio expression (a literal, an application, an operator expression). The synthesized type prints first (the `:t` view, rendered `expr : Type`), then the residual normal form (the `:normalize` view): expressions that touch host items commonly stay stuck as residual trees, and the type line is the reliable summary when they do. Type-check failures print the expression query's own diagnostic.

After the default action, a dim footer lists every command applicable to the matched kind(s), deduped across kinds — `(also: …)` — so the user discovers the other views of the same input. The footer presents each command under its full spelling (`:type`, `:references` — never a short form), in the order `:help` lists the commands, so the two surfaces present the command set identically. When the input matches **no** kind, the default action is dropped and a single honest line prints: `` `<input>` is not a name or a Kio expression — type :help for the command list ``.

The disambiguation rule is "shortest parse wins for the name kind": `foo` typed on its own is taken as a name (routed to `:doc`), not a zero-argument call. Use `:normalize foo` to force-reduce a name. Expression input resolves local declarations and explicit imports through the current scope; with no current module, a query has no expression scope and reports so.

**Argument-missing diagnostics.** Every argument-taking command (`:doc`, `:t`, `:pure`, `:source`, `:signature`, `:normalize`, `:load`, `:unload`, `:which`, `:refs`) invoked with no argument prints a per-command usage hint inline — the shape of the expected argument — rather than a generic "see `:help`" pointer.

**Named-item query trio.** `:signature`, `:source`, and `:t` are three fixed views of a single named item — its declaration header, its full source, and the type of the value it binds. Kiodoc exposes the same three as the `@signature` / `@source` / `@type` directives ([`kiodoc.md`](kiodoc.md) § Code-embedding directives), under one keyword vocabulary, one space-separated argument syntax, and one renderer per view shared with the REPL. The two surfaces differ only by sigil: `:` at the prompt, `@` in Kiodoc prose.

**Synonyms.** Several commands have a short and a long spelling, both first-class: `:l` / `:load`, `:u` / `:unload`, `:q` / `:quit`, `:h` / `:help` (and `:?`, a second short form for help), `:sig` / `:signature`, `:src` / `:source`, `:t` / `:type`, `:norm` / `:normalize`, `:ls` / `:list`, `:mods` / `:modules`, `:pkgs` / `:packages`, `:refs` / `:references`.

The declaration views `:doc`, `:signature`, `:source`, `:which`, and `:refs`
accept a nonminting forwarding label's braced selector, such as `{field}`,
`alias.{field}`, or `pkg/mod.{field}`. Its own declaration and documentation
are distinct from an ordinary function named `field`; reference lookup selects
label-syntax occurrences, not that function's value uses. Declaration-command
completion and terminal hints include the braced selector without adding a
synthetic uppercase type. Expression-oriented commands and bare-input
classification retain their existing rules: a braced label construction is
still an expression, and `:type` / `:pure` completion does not offer a
forwarding declaration as an ordinary value.

**Behaviors.**

- *Tab completion* offers `:command` spellings (including synonyms) at the start of a line, and the names the session knows — package module paths, loaded modules' item names, and item fully-qualified paths — in argument position. Command and declared-name discovery match *fuzzily*: candidates are ranked by match quality (an exact prefix outranks a case-insensitive prefix, which outranks a substring, which outranks a scattered subsequence), so `fac` finds `factorial`, `i32` finds every `*_i32` function, and `MAIN` finds `demo/main`. The candidate *set* is still the context-appropriate slice — a `:load` argument offers only module paths, never item names — fuzzy matching only ranks within that slice. Expression completion instead respects the language's type/value naming namespaces, lexical scope and parser-established context. Menus bound the displayed portion without discarding later matches. In the terminal, Tab cycles through candidates, Enter accepts a selection and Escape dismisses the menu.
- *Auto-reload*: a change to any `.kio` file in the directory tree triggers a re-typecheck. The check runs when the next command is submitted, so that command sees the current file contents; on success a `reloaded …` line prints, on failure a diagnostic prints and the session keeps the previous successful analysis. Auto-reload is always on.
- *History* persists across sessions at `$XDG_DATA_HOME/kio/history`, falling back to `$HOME/.local/share/kio/history`.
- Output is highlighted for the terminal — types, signatures, source — when standard output is an interactive terminal; it falls back to plain text on a pipe, a redirect, or a `dumb` terminal, and honors the `NO_COLOR` convention.

**Exit codes.**

- `0` — a clean exit (`:quit` or end-of-input).
- `1` (internal-error category) — the interactive terminal could not be opened, or the current directory could not be read.
- `2` (usage category) — `kio repl` was invoked with an unknown flag, or a startup selector that matches no module.

## `kio cache <subcommand>`

`kio cache` is a subcommand container for managing the package's Kio-semantic on-disk caches — the caches whose reads and writes the `--no-cache` global flag suppresses. Three children today: `kio cache clear`, `kio cache path`, and `kio cache gc`.

Kio records cache-entry access metadata under the configured cache root. A successful cache-using command may run a throttled garbage-collection pass for that root; failed commands do not trigger automatic GC. Retention is cache-family relative: for a cache family such as `typed`, `emit`, `doc`, or `equiv`, entries not accessed within the three days before that family's latest successful use may be removed. Host-toolchain caches outside the `--no-cache` contract are not pruned by this policy.

### `kio cache clear`

Clears the contents of the cache directory declared by the package's `build { cache "<path>"; }` block. Useful when a developer (or CI) wants to guarantee a fresh cache state without `rm -rf`'ing into implementation-specific paths.

**Behavior:**

1. Read `<name>.pkg.kio` from the current directory (the same package marker `kio build` uses) and parse its `build { ... }` block.
2. Resolve the `cache` field. If `cache ();`, or the package file has no build block, print `no cache configured` to stdout and exit `0` — the package has no cache directory to clear.
3. Otherwise, resolve `cache "<path>";` to a directory (relative paths are joined to the package root; absolute paths pass through). If the directory does not exist, print `cache at <path> was already empty` and exit `0` — a fresh checkout or a never-built package is a valid state.
4. Otherwise, remove every top-level entry in the cache directory except for the `rlib/` subdirectory (which is content-addressed by host-toolchain inputs and falls outside the `--no-cache` contract) and the cache root's `.gitignore` (the on-first-write marker that keeps the cache out of `git status`). Print `cleared <N> entries from <path>` to stdout, where `<N>` is the count of removed top-level entries.

`kio cache clear` takes no positional arguments and no flags (only `-h` / `--help`).

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the cache was cleared, was already empty, or the build block declares no cache (or there is no build block).
- `2` — CLI usage error (positional arguments, unknown flag, or no `<name>.pkg.kio` at the current directory — `kio cache clear` must be run from a package root, the same constraint `kio build` imposes).
- `1` — internal error: the package file couldn't be parsed, or a filesystem operation against the cache directory failed (e.g., permission denied).

### `kio cache path`

Prints the absolute path of the package's cache directory to stdout. The path is the same one [`kio cache clear`](#kio-cache-clear) operates on — resolved from the package's `build { cache "<path>"; }` block — so external tooling (CI scripts, profilers, editor integrations) can read or measure the cache contents without re-deriving the resolution rule.

**Behavior:**

1. Read `<name>.pkg.kio` from the current directory (the same package marker `kio build` uses) and parse its `build { ... }` block.
2. Resolve the `cache` field. If `cache ();`, or the package file has no build block, exit `40` (build error) with a diagnostic naming the package file — printing nothing would be ambiguous, and a script reading stdout deserves a loud signal that the package opted out.
3. Otherwise, resolve `cache "<path>";` to a directory (relative paths are joined to the package root; absolute paths pass through). Print the resolved path to stdout. The directory need not exist yet (a fresh checkout or a never-built package has a well-defined would-be path).

`kio cache path` takes no positional arguments and no flags (only `-h` / `--help`).

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the path was printed.
- `40` — no `<name>.pkg.kio` at the package root, or the build block declares `cache ();` / there is no build block.
- `2` — CLI usage error (positional arguments, unknown flag).
- `1` — internal error: the package file couldn't be read.

### `kio cache gc`

Garbage-collects stale Kio-semantic entries in the cache directory declared by the package's `build { cache "<path>"; }` block. The command applies the same retention policy used by automatic GC, but always performs the sweep instead of waiting for the throttled background trigger.

**Behavior:**

1. Read `<name>.pkg.kio` from the current directory (the same package marker `kio build` uses) and parse its `build { ... }` block.
2. Resolve the `cache` field. If `cache ();`, or the package file has no build block, print `no cache configured` to stdout and exit `0` — the package has no cache directory to collect.
3. Otherwise, resolve `cache "<path>";` to a directory (relative paths are joined to the package root; absolute paths pass through). If the directory does not exist, print `cache at <path> was already clean` and exit `0`.
4. Otherwise, prune known Kio-semantic cache families under that directory. Preserve `rlib/`, `.gitignore`, and unknown top-level entries. Print `garbage-collected <N> entries from <path>` when entries were removed, or `cache at <path> was already clean` when no entry was removed.

`kio cache gc` takes no positional arguments and no flags (only `-h` / `--help`).

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — garbage collection completed, the cache was already clean, or the build block declares no cache (or there is no build block).
- `2` — CLI usage error (positional arguments, unknown flag, or no `<name>.pkg.kio` at the current directory — `kio cache gc` must be run from a package root, the same constraint `kio build` imposes).
- `1` — internal error: the package file couldn't be parsed, or a filesystem operation against the cache directory failed (e.g., permission denied).

## `kio dep <subcommand>`

`kio dep` is a subcommand container for managing the current package's declared [dependencies](package.md#dependency-files) — the `<local>.dep.kio` files at the package root and the `<local>.lock.kio` pins beside them. Three children today: `kio dep fetch`, `kio dep update`, and `kio dep clean`. All are run from a package root. Materialization is a `kio dep fetch` / `kio dep update` step; `kio build` / `kio check` / `kio test` consume the materialized `<local>/…` trees as ordinary source, so the analysis pipeline stays dependency-agnostic. The materialized tree is **committed** to version control — a consumer ships its dependency's materialized closure (see [`package.md` § Dependency files](package.md#dependency-files)), so a fresh checkout already carries it.

### `kio dep fetch [--force] [<name>...]`

Materializes the package's dependencies on their own, **without a build**. For each dependency:

- A remote **`git`** dependency is fetched/cloned into the per-user cache (`$KIO_CACHE_HOME`, falling back to `$XDG_CACHE_HOME/kio` and then `$HOME/.cache/kio`), its commit is resolved and recorded in a committed `<local>.lock.kio` (on the first, unlocked resolve), the commit is checked out, and its modules are re-rooted under the `<local>/…` prefix. **An existing lockfile is honored** — a locked git dependency uses its locked commit, with **no re-resolution** — so `kio dep fetch` is reproducible (re-pinning a locked dependency is `kio dep update`'s job, not `fetch`'s).
- A local **`path`** dependency is materialized as usual (re-rooted under `<local>/…`); it carries no lock.

A Git source's optional `path` selects an exact `*.pkg.kio` manifest relative
to that checkout, with resolved containment inside it. Missing, wrong-kind,
or escaped targets fail at the selector before its first lock or materialized
tree is written; they never trigger fallback discovery. Without a selector,
the existing unique-manifest discovery at the root and one directory below
applies. The lock must match the URL, ref, and optional exact declared path;
adding, changing, or removing a selector requires `kio dep update` or restoring
the declaration, even when it would select the same package.

A `retype` counterpart's ordinary visibility must cover the source's export
surface and every import introduced or redirected by that remap. A visibility
failure is reported before writing or pruning that affected dependency's
module tree, including under `--force` and when its output is already current.
This is the [retype visibility guarantee](package.md#dependency-files), not
transactionality for other dependency errors or lockfile changes.

**Skip-if-already-materialized.** A dependency whose re-rooted module tree on disk already matches the lock's intent — every materialized module byte-identical to what re-rooting the lock's pinned commit (or, for a `path` dependency, the path source) produces, with no stale module left over — is a **no-op**: it is reported `` `<name>` (git|path): up to date `` and nothing is rewritten. The comparison is exact and against the lock's pinned commit (resolved with no re-resolution, and no network when the commit's checkout is already cached), so the skip is **sound**: a stale lockfile, a partial or corrupt checkout, a missing or extra module, or any byte difference is *not* up to date and falls through to a full (re)materialization. This elides the redundant re-fetch that an already-materialized dependency would otherwise repeat on every `kio dep fetch` (and on every implicit materialization a build / check / test performs).

`--force` bypasses the skip and re-materializes **every** selected dependency unconditionally — an explicit refresh, and the form a drift check uses to assert the committed materialization still equals a fresh fetch.

With no positional argument, every declared dependency is materialized. With one or more `<name>` arguments — each a dependency's `<local>.dep.kio` stem — only the named dependencies are materialized; a name that matches no declared dependency is a dependency error. The command reports each in-scope dependency: `` fetched `<name>` (git|path) `` when it was (re)materialized, or `` `<name>` (git|path): up to date `` when it was already current (and not forced). A package that declares no dependency prints `no dependencies declared` and exits `0`.

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the named (or all) dependencies were materialized or already up to date, or the package declares no dependency.
- `30` — a dependency error: a fetch / clone / ref-resolution or manifest-selection failure, a stale lockfile (its recorded `git` URL, `ref`, or optional `path` no longer matches the `.dep.kio`), a local-name collision, or an unknown named dependency.
- `2` — CLI usage error (unknown flag).
- `1` — internal error (e.g. the current directory cannot be read).

### `kio dep update [--allow-breaking] [<name>...]`

Re-pins git dependencies and re-materializes every dependency's tree. For each **`git`** dependency, re-resolves its `ref` to the commit it designates **now** and rewrites the `<local>.lock.kio` to that commit (and its fresh contract-surface digest), **ignoring** the commit the lockfile pinned before — the operation that advances a floating `ref` (a branch or tag) to its current commit. A **`path`** dependency has no lock to re-pin, but is re-materialized from its current on-disk modules like any other.

**The contract honesty gate.** Before rewriting an `A -> B` move, the dependency's contract surface at the old commit is compared against the new commit's, via the same compatibility relation [`versioning.md`](versioning.md) defines (the surfaces are recovered by checking out each commit and projecting its sealed `<pkg>.sig.kio` interface, or its bridge-reachable live surface when the dependency ships no sealed changelog). The verdict gates the re-pin:

- **compatible** → the lock is rewritten (commit + digest); the re-pin succeeds.
- **breaking** on a **sealed** dependency contract → a dependency error; the lock is left **unchanged** (the consumer stays pinned to the reproducible old commit), unless `--allow-breaking` downgrades the error to a warning and proceeds.
- **breaking** on an **unsealed** dependency contract (the dependency ships no `<pkg>.sig.kio`, or one still on its uncommitted first draft) → a warning, and the re-pin proceeds.

`--allow-breaking` only affects the sealed-break case; on a compatible or unsealed move it is inert. The breaking-change details (each dropped / narrowed export or added host requirement) are printed alongside the error or warning.

The comparison uses the same declared selector at both commits, or the existing
discovery rule when no selector is declared. A missing selected manifest aborts
the update without falling back to another package. A changed URL, ref, or
optional exact path is a fresh source pin, not an old/new comparison between
different sources. This includes selector changes at the same commit: they are
reported as a first lock, not as `unchanged`.

With no positional argument, every git dependency is re-pinned. With one or more `<name>` arguments, only the named dependencies (by their `<local>.dep.kio` stem); a name that matches no declared dependency is a dependency error. The command reports each dependency's outcome: `` `<name>` (git): <old> -> <new> `` for a moved pin (commits abbreviated to 12 hex chars), `` `<name>` (git): unchanged (<commit>) `` when the resolved commit equals the lock's existing pin, `` `<name>` (git): pinned <commit> (first lock) `` when no lockfile existed yet, or `` `<name>` (path): materialized `` for a path dependency. A package that declares no dependency prints `no dependencies declared` and exits `0`. `kio dep update` re-pins the lock **and** re-materializes every selected dependency's tree — the git half at the re-pinned commit, the path half from its current modules — so the consumer build reflects the move immediately. (It also checks out the old and new commits into the per-user cache to compute their contract surfaces for the gate.)

**The re-pin is atomic.** When several dependencies are re-pinned in one run, every re-pin is staged and the lockfiles are committed (and the trees re-materialized) only after **all** selected dependencies have passed their honesty gate. So a blocked sealed break (exit `30`) leaves **no** lockfile advanced — not even a compatible dependency processed earlier in the run — and therefore no committed tree stale against an advanced lock: the whole command leaves every dependency on its old commit, exactly as the single-dependency block already promised ("the lock was left unchanged"). Re-run with the break resolved, or with `--allow-breaking`, to advance.

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the named (or all) git dependencies were re-pinned (whether moved, unchanged, or first-locked, and including an unsealed or `--allow-breaking`-overridden breaking move), or the package declares no dependency.
- `30` — a dependency error: a fetch / clone / ref-resolution or manifest-selection failure, an unknown named dependency, or a breaking change to a **sealed** dependency contract that was not overridden with `--allow-breaking`.
- `2` — CLI usage error (unknown flag).
- `1` — internal error (e.g. the current directory cannot be read).

### `kio dep clean [<name>...]`

Removes the current package's materialized dependency trees from the working directory — the re-rooted `<local>/…` module tree each dependency was materialized into. The `<local>.dep.kio` declarations and any `<local>.lock.kio` pins are left untouched. A consumer commits its dependencies' materialized closure, so the removed tree is tracked: `kio dep clean` dirties the working tree, and `kio dep fetch` (or `git restore`) regenerates it. The command forces a clean re-materialization; it is not a way to keep a tree out of version control.

With no positional argument, every dependency's tree is removed. With one or more `<name>` arguments — each a dependency's `<local>.dep.kio` stem — only the named dependencies' trees are removed; a name that matches no declared dependency is a dependency error. Each removed dependency is reported (`` cleaned `<name>` ``), or `` `<name>`: nothing to clean `` when no tree was present. A package that declares no dependency prints `no dependencies declared` and exits `0`.

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the named (or all) dependency trees were removed, none were present, or the package declares no dependency.
- `30` — a dependency error: an unknown named dependency.
- `2` — CLI usage error (unknown flag).
- `1` — internal error (e.g. a tree could not be removed, or the current directory cannot be read).

**Whole-surface impact.** `kio dep` provides three CLI subcommands over the command-level dependency-materialization step (fetch / re-root / pin / remove), reading `<local>.dep.kio` and writing `<local>.lock.kio` and the committed re-rooted module tree:

- **Dependency and lock file syntax is shared by parser and tooling.** Git sources and pins admit an optional manifest `path`; the parser, `kio fmt`, Kiodoc file-kind snippets, parser-backed completion and semantic tokens, editor grammars, and the VS Code extension all recognize it. Completion offers each unoccupied source field, including `git` and `ref` after `path`, and suppresses occupied resolved fields. Strings and comments do not offer field completions. These files are not Kio' modules, so the independent **`kio-prime-check`** module grammar is unaffected.
- **The dependency's contract surface is consumed, not the consumer's.** The digest and the `kio dep update` honesty gate reuse the **`kio sig` compatibility layer** to project and compare the *dependency's* contract surface across commits. They do **not** change the consuming package's own host / bridge contract surface, its exports, or any type they reach, so they do not move what the **consumer's** `*.sig.kio` records — `kio sig` over the consumer is unaffected.
- **Module semantics are unchanged.** Materialization supplies ordinary modules to resolution, lowering, inference, `equiv` / `:normalize`, and backend emission; a Git manifest selector changes none of their rules or host APIs. Terminal and browser REPL completion is expression-shaped, not dependency-file completion. `kio-gen` generates programs rather than Git source declarations or lock files, so it is unaffected.
- **`kio dep fetch`'s skip-if-already-materialized fast path and `--force` flag are a CLI-surface change only.** The skip is a no-op short-circuit of the existing materialization step (it reads the on-disk re-rooted tree and the lock, writing nothing when they already agree); `--force` adds one boolean flag to the `fetch` grammar and bypasses the skip. This moves the **`kio dep fetch` argument grammar** (the `--force` flag — reflected in `--help`) and its **reported output** (the new `` up to date `` line), but defines **no new file shape, no in-source grammar, and no on-disk artifact**: `*.dep.kio` / `*.lock.kio` / the materialized tree are byte-for-byte what they were. The static shell completions enumerate the fixed subcommand grammar and the `dep` sub-subcommand *names*; like the existing `kio dep update --allow-breaking`, the `fetch --force` flag sits at the sub-subcommand level the completions do not separately list, so they are unchanged. Because materialization is already content-addressed (idempotent), the same skip applies to the implicit materialization a build / check / test performs, with no observable change beyond not rewriting unchanged files.

## `kio completions <shell>`

Prints a shell completion script for `kio` to stdout. The user installs the output per their shell's convention; the [shell-completions guide](../docs/guides/shell-completions.md) gives copy-paste install snippets for each shell.

`<shell>` is a required positional argument, one of:

- `bash` — emits a script defining a `_kio` completion function and registering it with `complete -F _kio kio`.
- `zsh` — emits an autoloadable `#compdef kio` function script.
- `fish` — emits a flat list of `complete -c kio` directives.

`kio completions` accepts exactly one `<shell>` argument and no flags (only `-h` / `--help`). A missing shell argument, an unrecognized shell, or extra arguments are a usage error.

**Completion behavior.** The generated scripts are **static**: they complete the fixed CLI grammar — the subcommand names, the `doc` / `cache` / `dep` / `sig` sub-subcommand names (including `dep clean` and `sig uncommit`), the `completions` `<shell>` names (`bash` / `zsh` / `fish`), and each subcommand's flags. Every other positional argument slot (a source path, a target id, a source selector, a module selector) falls back to the shell's default filename completion. The scripts do **not** introspect the package on disk to offer module names or target ids; completion is purely lexical. After upgrading `kio` — which may add a subcommand — the user re-runs `kio completions <shell>` to regenerate the script.

The completed subcommand set reflects the running binary's surface: a `kio` built without the `lsp` / `repl` features (see [`package.md`](package.md)) emits a script that omits those subcommands, the same way they are absent from `kio --help`.

**Exit codes.** Per [`exit-codes.md`](exit-codes.md):

- `0` — the completion script was printed.
- `2` — CLI usage error (no shell argument, unknown shell, unknown flag, extra arguments).

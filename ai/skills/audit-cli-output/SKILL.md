---
name: audit-cli-output
description: Verify all user-facing kio CLI output meets the Rust/Elm-grade bar — diagnostics (source context, secondary labels, help, suggestions), status/REPL/doc/lsp text, and --help conformance vs specs/cli.md; ANSI styling, surface vocabulary, user-error-vs-bug separation
allowed-tools: Read, Grep, Glob, Bash
---

# CLI-output audit

AGENTS.md § About Kio commits Kio to being **developer-friendly**: "great
diagnostics, fast feedback loops, and first-class tooling are part of the
design contract, not afterthoughts." This skill checks that **all**
user-facing `kio` CLI output reaches that bar — the standard Rust and Elm
set — and that new output sites don't regress below it.

"CLI output" = everything the `kio` command writes for a human to read:
**diagnostics** (errors/warnings, § A), **general output** (status,
progress, result, REPL, `kio doc`, `kio lsp` user-facing text, § B), and
**`--help`** (§ C). It is NOT about the *backend code* `kio` emits (that is
the per-backend emitter spec, covered by `audit-spec-drift` § 7 and the
`audit-backend*` skills). This skill is the covering audit for the
developer-friendly contract line.

Read `ai/topics/specs.md`, `specs/cli.md`, and `specs/diagnostics.md` —
together the authoritative diagnostics contract this audit enforces
(`diagnostics.md` pins a diagnostic's structured content and rendered
layout; `cli.md` pins the command surface).

## A. Diagnostics (errors and warnings)

### The diagnostic rubric

A great Kio diagnostic:

1. **Names the problem in surface/language vocabulary** — not
   implementation internals, and not a foreign language's syntax. A
   notorious bug class: rendering a Kio type binder as `<B>` (C++/Rust
   angle-bracket syntax) when Kio writes `[B]` for a binder declaration
   or just `B` for a use. NUANCE: `<X>` *is* Kio syntax for an
   **existential**, so it is correct only when the binder genuinely is
   existential; for a `forall`-bound binder use `[X]`, and where the
   distinction is unknown/irrelevant use bare `X`. Grep for
   `` `<{ ``-style type-var wrapping and check each against this rule.
   Cross-ref `audit-test-strategy` § goldens hygiene and `ai/topics/repo-layout.md`:
   messages/goldens must not name kio-rs functions/modules/types.
2. **Shows the source** — the offending line(s) rendered with a caret /
   underline under the primary span.
3. **Secondary labels** point at the *other* load-bearing spans (the
   binding site, the expected-type source, the conflicting declaration),
   not just the error point.
4. **`help:`** proposes a concrete fix when one is determinable.
5. **`did you mean …?`** suggestions where a near-miss is mechanically
   detectable (typo'd name vs. nearest in scope, wrong elaborator, missing
   import).
6. **User error vs. bug** — a user-facing diagnostic is a structured,
   span-bearing `Error`; an internal-contract failure is a `panic!` /
   `unreachable!`, never dumped to the user as a diagnostic (AGENTS.md
   § Bugs vs. input errors).

### A1. The Error type is rich enough

Inspect `kio-rs/src/error.rs`. The `Error` type must be able to *carry* a
great message: a primary `span` + `message`, plus secondary labels
(`Vec<(Span, String)>`), `help`, `note`s, and a structured `suggestion`
(a replacement span + text). A `{ span, message }`-only variant cannot
express the rubric — flag any error category still missing the rich
fields as a structural gap (not a per-message finding).

### A2. Construction sites use the available richness

Grep every `Error::… {` / error-constructor call site
(`grep -rn 'Error::' kio-rs/src/`). For each, by phase:

- **Type / Elaborator errors** (`14_type_error`, `15_elaborator_error`) — the
  highest-value for secondary labels: does the message point at *both*
  the mismatch site *and* the expected-type / coercion source?
  For imported callables, check direct and straight-alias calls against the
  actual written signature, including when its nominal type is defined in a
  third module. CLI related excerpts and LSP related locations must use the
  source snapshot that supplied the requirement. Keep argument-internal
  errors attached to their own constraints.
- **Name-resolution errors** (`13_name_resolution_error`) — the richest
  `did you mean` target: an unresolved name should suggest the nearest
  in-scope name when one is close.
- **Use / import errors** (`12_use_error`, `20_bridge_error`) — should
  name the conflicting declaration's span as a secondary label and
  suggest the fix.
- **Parse errors** (`11_parse_error`) — must not leak synthetic/wrapper
  tokens (e.g. a REPL `SyntheticWrap`'s `;`) the user never wrote.

Report sites that *could* carry a secondary label / help / suggestion per
the rubric but don't.

### A3. Rendering shows source context

Confirm the diagnostic renderer draws the offending source line with a
caret under the primary span (and underlines under secondary labels). A
renderer that prints only `file:line:col: message` with no source
snippet is the single biggest gap from the Rust/Elm bar — flag it.

Check tabbed source with primary and secondary spans, including tabs inside
an underlined span. Verify visible alignment in plain output, separately
from the CLI header's scalar columns and LSP UTF-16 ranges. Pin a renderer's
exact display arithmetic in its implementation fixtures, not as a universal
golden contract (`specs/diagnostics.md` § Two layers).

## B. General output (status, progress, result, REPL, doc/lsp)

The same developer-friendly contract covers *non-error* output too — what
`kio build` / `kio test` / `kio fmt` / `kio doc` / the REPL / `kio lsp`
print on the happy path.

### B1. No literal markup leaking into terminal output

User-facing terminal output must be **richly formatted with ANSI styling
where a TTY supports it**, never printed with literal markup tags. Grep
`println!` / `eprintln!` / output-builder sites in `kio-rs/src/` (and the
REPL / `cmd/*` / doc renderers) for literal Markdown/markup in strings
destined for the terminal — `**bold**`, `_italic_`, `#` headers,
`[text](url)`. Markup printed verbatim solely to request formatting from a
terminal that does not interpret it is a finding: it should be ANSI
styling (bold/dim/color/underline) or plain text instead. (Distinguish:
Markdown is correct *inside* `kio doc` HTML/Markdown output and in source
doc-comments — the finding is markdown in **terminal** status/diagnostic
strings.) Respect `NO_COLOR` / non-TTY: styling degrades to plain text,
never to raw escape codes or raw markup. Backticks delimiting inline code in
diagnostic messages, help, notes, and labels are permitted plain-text delimiters
under [`specs/diagnostics.md` § Color and TTY](../../../specs/diagnostics.md#color-and-tty);
they remain when ANSI styling is stripped and are not findings by themselves.

### B2. The message sweep

Walk the user-facing print sites (`kio-rs/src/cmd/*`, `repl/`, `lsp/`
user-facing messages, top-level driver, `doc` / help text). Each should:
read in surface/user vocabulary (not impl internals); be consistent in
tone/format with its siblings; use rich formatting appropriately; and not
contain stale/placeholder wording. Report messages that read as
developer-facing debug output rather than polished user output.

### B3. REPL / doc / lsp output

The REPL inspector (`:scope`, `:doc`, `:ls`, type/normalize output),
`kio doc` terminal rendering, and `kio lsp` user-facing messages are
first-class output surfaces — hold them to B1/B2 and the
surface-vocabulary rule.

## C. `--help` structural conformance (vs `specs/cli.md`)

`kio --help` and `kio <subcommand> --help` are the user's first contact
with the tool and must be consistent with `specs/cli.md` (the contract)
and structurally complete. This is a **mechanical conformance** check
(distinct from the quality rubric in A/B): it verifies presence and
spec-agreement, not prose quality.

### C1. Enumerate subcommands

The authoritative list lives in two places that must agree:

- The dispatch arms of `kio_lang::run` in `kio-rs/src/lib.rs` — every
  user-facing arm (excluding `debug` and `host-shape`, the intentionally
  unadvertised internal namespaces) is a public subcommand.
- The per-subcommand `## kio <name>` headers in `specs/cli.md`.

A subcommand missing from either side is a finding.

### C2. Per-subcommand `--help` shape

For each `kio <name> --help` (and each child of `kio doc` / `kio cache`):

1. **Synopsis line** — first line begins `Usage: kio <name>` with the
   same flag/positional shape the spec's per-subcommand header advertises.
2. **`Exit codes (per <URL>/specs/exit-codes.md):` block** — present,
   the URL is the pinned `KIO_DOCS_BASE_URL` prefix, and it enumerates
   every category code the subcommand can return (compare the spec's
   "Exit codes" subsection).
3. **Spec-footer URL** — ends `See <URL>/specs/cli.md#<anchor> …`, with
   `<URL>` = `KIO_DOCS_BASE_URL` and `<anchor>` the GitHub-style anchor.
4. **No bare `specs/` references** — every reference is the full GitHub
   URL form, never a bare relative `specs/<file>.md` path.

### C3. Flag / identifier agreement

For each subcommand: every flag in the spec's per-subcommand section must
appear in `--help`, and every flag in `--help` must appear in the spec.
The check is on **identifiers**, not prose (the spec describes behavior,
`--help` describes mechanics — wording diverges intentionally). The
`--no-cache` global appears in top-level `kio --help` "Options:"; it is
one-sided (per-subcommand blocks may but need not repeat it).

### C4. Top-level `kio --help` / `kio-prime --help`

Every dispatcher-accepted subcommand (excluding `debug` / `host-shape`)
appears under "Subcommands:"; the `See <URL>` footer is present;
`--no-cache` and `-V` / `--version` are under "Options:". Internal/debug
subcommands stay undocumented in both `--help` and `cli.md` — confirm they
did not leak.

## D. Goldens pin output quality

The `*_error` / `*_failure` golden buckets'
`expected.stderr` (or `.grep`) files should pin the *quality* of the
message, not just its category; `expected.stdout` for happy-path goldens
should pin the formatted output shape (without over-fitting incidental
wording — balance against `.grep` flexibility); and the help-text goldens
should pin the four §C2 structural elements. A bucket whose goldens only
grep the category gives no regression protection for output quality —
flag under-specified goldens.

## E. New output sites land to the bar

Run `git log --since="3 months ago" --diff-filter=A -G 'Error::|println!|eprintln!'
-- kio-rs/src/`. A newly-added error as bare `{ span, message }`, or a new
print site emitting literal markup / debug-flavored wording, is a
regression of the contract — catch these as they land, before the backlog
regrows.

## How to report

Group findings:

1. **Structural gaps** — error categories whose type can't carry the
   diagnostic rubric (missing secondary/help/note/suggestion fields).
2. **Rendering gaps** — no source-context / caret rendering.
3. **Foreign-syntax / wrong-vocabulary** — diagnostics spelling Kio
   constructs in another language's syntax (the `<B>`-for-`[B]` binder
   bug class, respecting the `<X>`-existential nuance), or naming impl
   internals.
4. **Literal-markup-in-terminal** — output printing formatting markup such
   as `**…**` verbatim instead of ANSI styling or plain text, excluding the
   diagnostic code delimiters specified in §B1.
5. **Per-site backlog** — diagnostic + general-output sites that could
   meet the rubric but don't, prioritized (type/elaborator/name-res first for
   diagnostics).
6. **`--help` conformance** (§C) — per subcommand: missing synopsis /
   exit-codes block / spec footer; bare `specs/` references; flags in the
   binary not in `cli.md` (or vice versa); subcommands missing a help
   block (or help blocks for undocumented subcommands); internal/debug
   subcommands that leaked into `cli.md` / `--help`.
7. **Golden gaps** — buckets whose goldens don't pin output quality.
8. **New-site regressions** — recently-added bare error / raw-markup
   output sites.

For each finding, cite the code file/line (and golden) and which rubric
point it misses.

**Default: report only.** If invoked with a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

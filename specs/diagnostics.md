# Kio diagnostics

This document specifies the **structured content** of a Kio compile-time
diagnostic and the **rendered layout** an implementation produces from
it. It is a companion to [`exit-codes.md`](exit-codes.md): exit codes
report the *category* of an error (parse, type, elaborator, …); this page
specifies what a single diagnostic *contains* and how it is drawn.

A diagnostic reports a **user error** — a program the user could fix by
editing their source. It is never the channel for an implementation
**bug**: an unexpected internal condition the implementation didn't
plan for terminates with the internal-error exit code (`1`) and is not
dressed up as a span-bearing diagnostic. Keeping the two apart is a
hard implementation rule; this page governs only the user-error channel.

## Two layers: structured content vs. rendered text

Kio implementations differ in their exact wording and terminal
styling. To keep the contract testable across implementations, this
page draws a line:

- **Structured content** — the fields below (primary span, message,
  secondary labels, help, notes, fixes). Two conformant
  implementations diagnosing the same ill-formed program must agree on
  this content up to incidental wording: the *same* spans carry labels,
  a help line is present when the rule says one is, and a fix offers the
  *same* replacement over the *same* span.
- **Rendered text** — the concrete lines an implementation prints. The
  layout below is the recommended form (and the one kio-rs produces),
  but the exact glyphs, color, and column arithmetic are incidental.
  Goldens that pin rendered text are implementation-specific fixtures,
  not cross-implementation contract.

A test that wants to assert error *category* across implementations
pins the exit code. A test that wants to assert a *fix* exists pins the
structured fact ("offers `bar` for `baf`"), not the byte layout.

## Structured content

A diagnostic carries:

| Field | Type | Meaning |
|---|---|---|
| **primary span** | a source span | the offending location — where the caret points |
| **message** | text | names the problem in the user's surface vocabulary, not implementation internals |
| **secondary labels** | list of (optional file, span, text) | the *other* load-bearing locations: the binding site, the expected-type source, the conflicting declaration. No file means the diagnostic's primary file. |
| **help** | optional text | a concrete proposed fix, phrased as guidance |
| **notes** | list of text | clarifying context that is neither the primary message nor tied to a span |
| **fixes** | list of fixes | zero or more alternative edits. Each fix has a title, an applicability (`machine-applicable` or `maybe-incorrect`), and one or more text edits. Each edit carries an optional file, a source span, and replacement text; no file means the diagnostic's own file. |

The **primary span** and **message** are mandatory; every other field
is optional and defaults to empty. A minimal diagnostic is exactly the
`{primary span, message}` pair — the shape every Kio diagnostic had
before this contract, now the floor rather than the ceiling.

A **fix** is distinct from **help**. Help is prose the user reads; a fix
is a structured edit a tool can apply. The `did you mean <name>?` shape
pairs a machine-applicable fix (replace the typo'd span with `<name>`)
with help text that states the same fix in prose. A diagnostic may carry
one without the other.

### What each field is *for*

- The **message** names the problem. It must read in surface
  vocabulary — e.g. `expected Foo, found Foo -> Foo` — not in terms of
  the implementation's internal representation. Goldens stay
  implementation-agnostic, so message wording is never the
  cross-implementation oracle (that is the exit code's job).
- **Secondary labels** are the highest-value enrichment for type and
  elaborator errors: they point the caret at the *expected-type source* and
  the *mismatch site* simultaneously, so the user sees both ends of a
  conflict at once. For name-resolution errors a secondary label points
  at a conflicting prior declaration.
  An imported callable's written signature remains owned by its declaring
  source through a straight local alias. Its expected-type label identifies
  that signature or annotation, not the nominal type's definition.
- **help** proposes the fix the implementation is confident about
  ("import a type marked `role(bool)`").
- A **machine-applicable fix** fires only when a near-miss is mechanically
  detectable (a typo'd name within edit distance of an in-scope name, a
  wrong elaborator form whose sibling would succeed, a missing import the
  resolver can name).
- A **maybe-incorrect fix** is an editor convenience for edits that are
  syntactically plausible but may erase deliberately sequenced
  computation, comments, or design intent. Tools may offer it, but should
  not apply it automatically.

### Invalid source characters

Invalid-token and invalid-string-escape diagnostics identify the complete
source character and span its whole UTF-8 encoding, not a single byte of a
multibyte character. This does not broaden the ASCII identifier grammar or
the admitted string escape sequences.

### Declaration context and value-shaped type syntax

A malformed module header with adjacent names explains that `module` takes
one slash-separated path, relative to the package root with `.kio` removed,
without a package-name prefix. Keyword spellings remain legal module path
segments. A package declaration in a regular module explains that package
headers belong in `*.pkg.kio` files. These diagnostics do not guess an
intended module path from the invalid header.

Recognizable file-level headers in a regular module identify the file kind
that owns the declaration. A missing declaration or statement terminator
names its enclosing construct; malformed declaration headers describe the
expected form without reserving contextual identifiers. A host function
body explains that the host supplies the function and that its declaration
ends with a semicolon. Package-file ordering diagnostics do not imply that
an optional block was present when it was absent.

In type position, empty value parentheses are diagnosed with the unit type
spelling `.`. A balanced tuple-shaped group offers a structured product-type
repair when all its components parse: each top-level comma becomes `&`, with
component grouping and nested type-argument commas preserved. Zero-component
groups instead offer the unit type. Repairs preserve comments and separate
replacement tokens from adjacent operators. An incomplete or malformed
component does not receive a partial product repair. These explicit edits do
not admit value-shaped syntax as types.

### Value names in type references

A value-name spelling in a syntactic type reference is a parse error. Its
message distinguishes the value-name spelling from the required type name,
and a note preserves the type-name spelling rule. This is a syntactic
classification, not evidence that a value declaration exists; the diagnostic
does not infer a binding or point to a guessed declaration.

### Type names used as values

When ordinary lookup identifies an available type binding at a rejected
value reference, the diagnostic distinguishes that type from a missing
value and identifies its declaration kind and location. This follows the
actual lexical or imported binding, including visibility and source order;
capitalization alone does not prove a declaration exists. A known type in
value position is not an unresolved-name candidate for an import or a new
function stub.

## Rendered layout

The recommended rendering, and the one kio-rs emits, draws the
structured content as:

```
<path>:<line>:<col>: <message>
  |
N |     <the offending source line>
  |     <spaces>^^^^ <primary label, if any>
  |
N |     <a source line carrying a secondary label>
  |     <spaces>---- <secondary label text>
  |
  = help: <help text>
  = note: <note text>
  = suggestion: replace with `<replacement>`
```

Rules:

- The **header** is `path:line:col: message`, with `line`/`col`
  1-based, the column counting Unicode scalar values (a multibyte
  character is one column). This single line is the stable spine every
  earlier Kio diagnostic already emitted; the snippet and trailer below
  it are additive.
- Each **source-line block** prints the gutter (`` ` N | ` ``) followed
  by the source line, then an underline row (`` `   | ` `` gutter, spaces
  to the span start, then a run of carets). The **primary** span underlines
  with `^`; a **secondary** span underlines with `-`. A label's text,
  when present, trails its underline.
  Tab handling is consistent between the displayed source and underline;
  tabs do not leave a caret or secondary mark pointing into unrelated
  whitespace. Display expansion does not change the header's source columns.
- A secondary in another file starts with
  `` `--> <path>:<line>:<col>` `` and uses that file's own source text,
  line index, and gutter. If the exact related source is unavailable or the
  recorded span is invalid for it, the renderer prints
  `` `--> <path>: source unavailable: <label>` ``. It never interprets a
  foreign span against the primary file.
- Spans confined to a single line underline directly. A span covering
  more than one line underlines from its start column to the end of its
  first line; the header line/col already names the start, which is the
  actionable position.
- The **trailer** prints `help`, then each `note`, then the first
  machine-applicable single-file edit as `suggestion`, each on its own
  `= <kind>: …` line. The `suggestion` spelling is the terminal-rendered
  compatibility label for that fix; the structured field is still
  `fixes`. Maybe-incorrect fixes and multi-file fixes are left to tool
  integrations such as the LSP code-action surface. The trailer is
  omitted entirely when all three are empty.
- **Ordering** is deterministic: header, then the primary block, then
  primary-file secondary blocks in span order, then cross-file groups by
  displayed path (exact path breaks a display tie) with labels in span order,
  then the trailer. Determinism keeps the rendered form reproducible for
  fixtures.

### Color and TTY

Color is **incidental** styling, never load-bearing — the same
diagnostic must be unambiguous in plain text. An implementation that
colors its output:

- emits no color when the stream is **not a terminal** (piped,
  redirected, captured by a test harness), so captured output is plain;
- emits no color when the **`NO_COLOR`** environment variable is set
  to any value, honoring <https://no-color.org/>;
- otherwise may style the header, carets, and labels.

Inline `` `code` `` spans inside the message, help, note, and label text
are markup *only for styling*: a coloring implementation may render the
run between a backtick pair in a distinct style (kio-rs uses bold), but
the backticks themselves remain in the plain-text rendering — they are
the plain delimiter, not a Markdown directive to be consumed. A
non-color rendering is therefore byte-identical to one with the styling
stripped.

Because color is suppressed for non-terminal streams, golden fixtures
capturing stderr see plain text regardless of the implementation's
terminal behavior.

## Relationship to other specs

- [`exit-codes.md`](exit-codes.md) — the category/exit-code mapping. The
  reported diagnostic's category determines its exit code; when independent
  errors coexist, neither document selects which one must be reported.
- [`cli.md`](cli.md) — which commands emit diagnostics and on which
  stream (diagnostics go to stderr; see `cli.md`).
- [`kiodoc.md`](kiodoc.md) — `check_exit_code` lets a documentation
  snippet assert the *category* of an expected failure without pinning
  rendered text, consistent with the two-layer split above.

# Language Surface Tooling

Trigger: changing language syntax, parser tokens, formatter output, user-facing grammar, or language semantics; editing LSP, REPL, token classification, editor grammar/extension code, or completion tests and browser completion code.

## Gate 0: establish contract authority

Before implementation, state the exact normative behavior delta, trace its authority to the effective authorized contract — the authority-baseline specification plus any specific user instruction or approval given before implementation — and name the behaviors that must remain unchanged. A delegated task must cite the authority it inherits and cannot broaden it. Fix the baseline at the start of the user-requested work, before agent-authored edits; commits, branches, sessions, and same-work spec edits do not reset it. A current implementation, new test, review, or audit finding is evidence rather than authority. If a change to accepted programs, resolution, inference, visibility or capabilities, specified diagnostics, or a phase boundary is not uniquely authorized, stop and ask the user to decide before implementation. If an unauthorized prototype already exists, remove it from the landable range; a later decision starts newly authorized work and does not waive fresh review or the rest of this checklist. Incidental or explicitly unspecified implementation details are outside this gate. Do not infer a diagnostic-selection order from the exit-code table: absent an explicit command-specific rule, selection among independent errors is deterministic but has no category precedence, and public conformance tests that pin a category isolate one defect.

## Impact checklist

A surface or semantic change is not complete until the language and tooling agree on the same accepted spelling and the same user-facing behavior. Before implementation or landing, state the impact argument: which bridge surfaces must change, which are intentionally unchanged, and why "unchanged" follows from the boundary being modified.

Use that argument to choose the affected surface area:

- Parser and parse diagnostics: accepted forms, rejected old forms, recovery, and targeted suggestions where practical.
- Formatter and style contract: canonical spelling, list layout, shorthand expansion/collapse, and stable round trips.
- Lowering boundary: every surface-only form disappears before Kio'; phase-local elaboration data must be consumed before the boundary closes.
- Typechecker behavior: synthesis/checking rules, expected-type flow, open-world argument when name resolution, imports, dispatch, or inference are involved.
- Normalization behavior: `equiv`, REPL `:normalize`, user elaborator execution, and any compile-time evaluator path that observes the changed semantics.
- Generated corpus: whether `kio-gen-rs` can still generate well-typed programs for the changed language and whether it should generate the new shape.
- File-kind surface parity: every Kio-family file kind in the shared file-kind vocabulary must be considered together, not by a hard-coded module/package subset. Check `kio fmt`, `kio doc` `variant=KIND`, highlighters, LSP routing, IDE plugins, syntax fixtures, and docs against the whole set.
- LSP: diagnostics, formatting, semantic tokens, hover, completion, code actions (including the source they generate), definition, references, prepare-rename, and rename. File-kind routing must follow the shared Kio-family predicates rather than ad hoc suffix lists.
- REPL: command parsing, scope/output, expression queries, completion, highlighting, and multiline input; distinguish the shared inspector core, terminal front end, and browser wrapper.
- Editor grammars and extensions: tree-sitter, TextMate, the VS Code extension's bundled grammar/configuration, fixtures, and highlight corpus coverage; keep them consistent with `kio debug tokens` and cover every Kio-family file kind.
- Contract-surface changelog: `kio sig` records the host/bridge contract surface — a change to the host boundary, exports, or the types they reach moves what a `*.sig.kio` records; see [`specs/versioning.md`](../../specs/versioning.md).
- Public contracts: update `specs/grammar.md`, `specs/language.md`, `specs/prime.md` when the grammar or lowering boundary changes; update `specs/style.md` for formatter behavior and `specs/cli.md` for LSP/editor behavior.
- Public docs: update affected guides, tutorials, and runnable snippets in `docs/`.
- Tests: add focused parser/formatter/type/normalizer/LSP/highlight/VS Code coverage plus success and negative goldens for the new behavior and any rejected old behavior.

For syntax touching row forms, labels, field access/update, imports, dispatch, or inference, state the open-world argument explicitly before implementing the rule.

## Completion currency

Completion is bidirectional: identifier candidates equal the bindings in the cursor's lexical/source-order scope and selected namespace, with shadowing and ordinary import visibility preserved. At every position where the next user-selectable named atom comes from a closed language-defined vocabulary, contextual completion equals the choices that can still lead to an accepted, canonically spelled construct. This includes all five Kio-family file kinds, not only modules and packages. Reserved, removed, rejected, duplicate, mutually exclusive, order-invalid, and context-invalid choices are not suggestions. Parser recognition for a targeted diagnostic does not make a choice eligible; derive eligibility from the effective grammar, semantics, package/versioning and target-key contracts.

Trace the shared context/scope machinery through each actual LSP, terminal REPL, and browser/wasm completion entry point. Suppress unrelated identifier suggestions in comments, literals, and binder-introduction positions; distinguish language-name matching from command discovery. Replacement ranges cover the complete started identifier or operator run. Candidate labels and optional type/documentation enrichment must retain the exact selected binding, provider edge, and current source version. Omit unauthenticated enrichment rather than attaching a same-spelled or stale declaration's metadata. Bounded menu presentation does not authorize discarding eligible matches; apply the response and menu contracts in `specs/cli.md` separately.

An authorized change to eligibility moves the affected providers and focused evidence together: exact positive/negative candidate sets, insertion into an otherwise-valid continuation, and edit-driven retraction. Cover document/provider edits, shadowing, incomplete intermediates, and close/reopen or session reload. Shared collector tests do not substitute for the distinct JSON-RPC, interactive terminal, and browser completion boundaries. Static audits inspect implementation and existing evidence; execution follows the scoped validation procedure below when requested or required by an authorized fix.

## Focused tooling validation

Before the expected-green broad gate, run the affected executable tooling suites
listed in [`local-ci.md` § Language and editor-tooling preflight](local-ci.md#language-and-editor-tooling-preflight).
Library tests, parser goldens, lexical agreement, and real LSP/editor integration
tests exercise different boundaries; one does not substitute for another.
Include embedded Kio in Rust/JS/TS test fixtures and source-producing features,
not only `.kio` files. A syntax migration must preserve each fixture's actual
subject. Apply and reanalyse a code action's edit, checking the intended syntax
and semantic result while preserving any deliberately remaining diagnostics.

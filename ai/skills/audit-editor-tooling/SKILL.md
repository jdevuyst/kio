---
name: audit-editor-tooling
description: Audit non-completion LSP request semantics, editor grammar integration, and VS Code lifecycle correctness from source and existing client evidence.
allowed-tools: Read, Grep, Glob, Bash
---

# Editor tooling audit

Anchors: [`language-surface-tooling.md` § Impact checklist](../../topics/language-surface-tooling.md#impact-checklist)
and [§ Focused tooling validation](../../topics/language-surface-tooling.md#focused-tooling-validation),
with the public contract in [`specs/cli.md` § `kio lsp`](../../../specs/cli.md#kio-lsp).
Read these before assessing behavior. Implementation and tests are evidence,
not authority to revise the contract.

**Default: read-only static audit.** Inspect source, assertions, and existing
execution receipts; do not build or launch servers, editors, or integration
harnesses. Report unexecuted behavior as unverified. Creating or invoking this
skill does not itself establish editor acceptance.

Keep adjacent ownership explicit: `audit-lsp-completion` owns completion
eligibility, insertion, and presentation across clients; `audit-cli-output`
owns diagnostic quality and native diagnostic-fix discovery/application;
`audit-compiler-performance` owns scheduling/resource costs. Consume their
findings or identify the needed cross-check, rather than rerunning those audits
implicitly. This skill owns remaining request semantics and editor integration;
non-diagnostic refactorings remain in scope.

## 1. Contract and consumer inventory

Build a compact request/consumer matrix from both the public CLI contract and
`kio-rs/src/lsp/mod.rs` capabilities and dispatch. Include notifications and
shutdown, not only text-document requests. For each row, record the contract,
handler, source/typed-snapshot dependency, applicable file kinds and clients,
and existing positive/refusal evidence. Capability advertisement must agree
with actual handling; do not freeze the inventory to a count in this skill.

Follow handlers in `kio-rs/src/lsp/`, shared identity/scope providers, and
`kio-rs/src/tokens.rs`. Use `kio-rs/src/file_kind.rs` for the complete file-kind
set. Inspect the VS Code manifest and `tools/vscode-kio/src/extension.ts` and
`src/lsp.ts`, plus bundled tree-sitter/TextMate assets and their source owners.
Separate local/remote Node language services from browser-only highlighting.

## 2. Non-completion request semantics

Check the request-specific rules in `specs/cli.md`, including:

- Hover, definition, references, and highlights select the exact binding and
  namespace. Cover shadowing, imports/provider edges, type parameters, nominal
  members, and continuation labels; same spelling is not shared identity.
  Check reference ordering and `includeDeclaration`, null/refusal behavior,
  and documentation/signature ownership.
- Configuration field hover describes accepted forms and defaults from shared
  field definitions, using the current syntax owner rather than a same-spelled
  key elsewhere. Check grouped values and invalid/incomplete contexts; these
  descriptions are not ordinary Kio types. Completion remains owned by
  `audit-lsp-completion`.
- Prepare-rename and rename preserve binding ownership across the package,
  reject invalid names and capture/collisions, and respect nonrenameable and
  package-external boundaries. Include row-let shorthand, numbered binding
  families, and label identities where applicable. Inspect actual versioned
  edits and reanalysis, not only replacement strings.
- Document symbols, folding ranges, inlay hints, and signature help retain
  their specified hierarchy, ranges, supported contexts and refusal cases.
  Public call types and written syntax determine argument display; private
  elaborator ABI grouping is not source-call authority.
- Formatting follows `kio fmt` for the selected file kind and current overlay,
  preserving the documented parse-error no-op and URI rules. Semantic tokens
  use the advertised legend and valid UTF-16 delta encoding, including
  multiline/doc-comment content and incomplete-source fallback. Syntax-only
  classification must not borrow an import provider's grammar.
- Non-diagnostic code actions respect kind filtering, selected source and
  binding identity, versioned edits, source/comment preservation, semantic
  equivalence, and stale/ambiguous refusal. Route diagnostic fixes to
  `audit-cli-output`; sharing the transport does not make a refactoring a fix.

For every applicable row, trace document open/change/save/close and provider
edits through snapshot selection. Apply incremental edits in their defined
order, preserve overlay authority and disk fallback, and distinguish syntax
responses, focused typed shards, and package-wide typed answers. Check the
contract's permitted latest-snapshot response behavior rather than inventing
a blanket synchronous-freshness requirement. Stale facts must not authorize
edits or metadata where the request requires current sources. Validate URI
authentication, byte/UTF-16 boundaries, recovery behavior, cancellation, and
publication/version handling at their actual consumers.

## 3. Editor grammar and lifecycle integration

Trace file selectors, packaged assets, token legends, and contextual roles
through tree-sitter, TextMate, and the actual VS Code providers. Compare with
compiler-derived classification while retaining documented differences such
as a line-local grammar's limited context. Include all Kio-family file kinds,
ordinary-name inverses for contextual keywords, incomplete buffers, and
embedded documentation. Parser/token unit tests do not prove that the shipped
extension bundles or registers the right provider.

Audit the language-server lifecycle against the CLI contract: executable
selection and invalid explicit selection; bounded probe/initialization;
connected, recovering, unavailable and unhealthy states; bounded restart;
settings changes and manual recovery; and deactivation during startup.
Distinguish transport closure from process exit. Inspect child termination,
pending-start cancellation, status/notification deduplication and unhandled
failure handling. Recovery needs diagnostics from the real selected compiler;
ordinary source errors, expected request rejection, and version-number
differences must not become availability warnings. Browser-only highlighting
must not report a missing local compiler.

## 4. Evidence and bounded execution

Trace assertions through `kio-rs/tests/lsp_smoke.rs` and its modules for real
JSON-RPC behavior, and `tools/vscode-kio/test/suite/` for the actual editor
provider and lifecycle boundary. Inspect test registration, fixtures, binary
selection and extension packaging, including
`ci/checks/orchestrators/lsp-tests.sh`, `vscode-e2e.sh`, and the highlight
orchestrators. A mock server can inject a failure but does not prove real
compiler recovery. A harness exit alone does not prove child cleanup, provider
application, or absence of unhandled failures.

Record source/compiler/extension identity, executed assertions, platforms and
client modes for existing receipts. Unit, JSON-RPC, actual-editor, and
installed-package evidence are distinct; a source-built desktop test does not
certify remote/browser behavior or a published VSIX. Mark missing or skipped
cells explicitly, without treating unexecuted tests as failures of the product.

Only for explicitly requested execution or validation of an authorized fix,
read [`local-ci.md` § Language and editor-tooling preflight](../../topics/local-ci.md#language-and-editor-tooling-preflight).
Select the smallest real boundary proving the claim, verify supported filters
and nonzero executed counts, and retain causal failure/repair evidence for a
fix. Follow the Cargo/cache/temporary-storage and aggregate-scope gates. Do
not launch a broad suite, matrix, or `ci/all.sh` merely to complete this static
audit; missing tools or client platforms are reportable evidence gaps.

## 5. Report

Group contract/implementation mismatches, identity/edit defects, lifecycle or
packaging defects, and evidence gaps. Give exact anchors, source state or event
sequence, expected versus observed behavior, and receipt where available.
Distinguish inspected code from executed evidence and name adjacent audit
ownership. Do not claim a full editor pass from grammar agreement alone.

For a fix directive, follow [`audit-fix-mode.md`](../../topics/audit-fix-mode.md).
An uncovered contract or missing test does not broaden repair authority.

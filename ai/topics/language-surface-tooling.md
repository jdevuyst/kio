# Language Surface Tooling

Trigger: changing language syntax, parser tokens, formatter output, user-facing grammar, or language semantics; editing LSP, REPL, token classification, editor grammar/extension code, or completion tests and browser completion code.

## Gate 0: establish contract authority

Before implementation, state the exact normative behavior delta, trace its authority to the effective authorized contract — the authority-baseline specification plus any specific user instruction or approval given before implementation — and name the behaviors that must remain unchanged. A delegated task must cite the authority it inherits and cannot broaden it. Fix the baseline at the start of the user-requested work, before agent-authored edits; commits, branches, sessions, and same-work spec edits do not reset it. A current implementation, new test, review, or audit finding is evidence rather than authority. If a change to accepted programs, resolution, inference, visibility or capabilities, specified diagnostics, or a phase boundary is not uniquely authorized, stop and ask the user to decide before implementation. If an unauthorized prototype already exists, remove it from the landable range; a later decision starts newly authorized work and does not waive fresh review or the rest of this checklist. Incidental or explicitly unspecified implementation details are outside this gate. Do not infer a diagnostic-selection order from the exit-code table: absent an explicit command-specific rule, selection among independent errors is deterministic but has no category precedence, and public conformance tests that pin a category isolate one defect.

## Consistent syntax across contexts

Reuse established syntactic conventions wherever the same construct appears.
A context-specific restriction must express a deliberate language distinction,
not arise incidentally from a separate parser. This principle informs design
and review; Gate 0 still determines authority for each acceptance change.

Compare block fields by semantic role, not by which declaration contains them:
stored scalar values, references to existing declarations, declarations of new
names, and collections are distinct roles. Inventory accepted syntax, optional
absence/defaults, comments and tooling for each role across configuration and
language declarations. Share parsing and field metadata where their contracts
agree; justify differences from their meaning, not separate parser branches.
Grouping is one consistency witness, not the whole review.

Configuration field definitions in `config_fields.rs` and `build_target.rs`
share accepted scalar forms, finite choices and omission defaults with parsing
and tooling. Derive key/value completion and field descriptions from those
definitions, not duplicated per-field menus or invented compiler `Type` values.
Keep syntactic presence distinct from effective defaults: formatting preserves
written optional fields and does not insert omitted ones, except that a host
target's omitted or Unit `baseline` materializes the concrete latest baseline
from the fixed compiler catalog. Explicit `latest` stays floating and concrete
selectors stay fixed. Test that exception, comment preservation, key retraction,
grouped values, and insertion of suggested values in their actual owner context;
required fields and conditional source alternatives retain their constraints.

For grouping parentheses, enumerate contexts that consume values or types,
including configuration defaults and the Kio-family file kinds. Compare an
accepted value or type with its grouped and nested-grouped spelling: grouping
must be transparent when the context accepts that same construct. Parentheses
forming argument lists, patterns or operator grammar can have a distinct role;
classify those roles before claiming an inconsistency. Record each deliberate
restriction and its contract evidence. Do not claim uniform acceptance from
the principle alone or turn this check into an unrelated syntax redesign.

Prefer shared grammar handling for the same construct, with focused contextual
tests for acceptance, meaning, diagnostics and formatter round trips. A shared
parser test alone does not prove that every contextual entry point uses it.
[`audit-spec-drift`](../skills/audit-spec-drift/SKILL.md) checks contract and
parser agreement; [`audit-test-strategy`](../skills/audit-test-strategy/SKILL.md)
checks the context witnesses and their actual assertions.

## Readable grouping

Apply [DESIGN.md's grouping principle](../../DESIGN.md#readable-grouping)
to proposed rules as well as existing syntax. Explain the grouping from
written boundaries and declared composition permissions, not the parser's
control flow. Distinguish ordinary structural composition from operator
associativity, chain-slot permissions and final-word atoms; test how those permissions
interact rather than checking each only in isolation.

For overlapping continuations, closing tokens or competing chain directions,
state the public boundary rule and show parenthesized alternatives. Flag a
choice made only by branch order, lookup order or successful trial parsing.
Explicit permissions can still compete; their existence alone does not settle
ownership. Keep syntactic grouping separate from type-directed argument
planning after that grouping is fixed. Gate 0 still governs contract changes;
an audit must report an unsettled rule rather than silently choose one.

For trailing-block UFCS, distinguish receiver insertion among prefix values
from projection of the final block arguments. Exercise all four directions,
post-block reverse placement, grouping without a second splice, tight receiver
stops and result chaining. Both reverse directions require explicit braces
around the selected call's own final labelled child, even through grouping of
the whole call. The brace-insertion fix preserves deterministic outer
attachment, never relocates the receiver, and conserves comments. Formatter
elision must retain these braces and those protecting a spliced child, while
direct/forward chains and deeper ordinary chains inside explicit blocks keep
their shorthand. Compare omitted, empty and explicit-Unit prefix lists without changing
the direct block call's Unit meaning.

Use focused grouping and meaning-preserving formatter witnesses at affected
boundaries, including nested and grouped controls. Where custom operators are
involved, also check independent import-order permutations and unrelated
provider additions under the existing open-world contract. Parsing successfully
and formatting idempotently do not prove that grouping is correct.

For operator changes, retain the complete structural classifier: arbitrary
slot counts do not waive ineffective-`__` rejection or conflicting exposed
ends. Exercise maximal leading extent before operand checks, opposing claims,
native suffix ownership and macro-head grouping. Frame identity follows structural
square edges and whole mirrored marker words before binding lookup; a failed
parse or missing binding cannot select a shorter marker. Keep value-only
slots/elements value-only, with explicit type arguments confined to their
singleton bracket stages.
The shared lexical rules also apply to independent Prime parsing and all five
file kinds; dot/bracket roles, signed literals and comment ownership need
their own witnesses.

## Call stages and bracket ownership

Keep explicit value-level type arguments in square suffixes and ordinary value
arguments in parentheses. Each type-supply or binder bracket contains exactly
one entry: declaration/prefix binders, value instantiation, TypePath arguments
and literal/literal-alias carriers all follow that rule. Reject comma lists
and empty brackets before formatting; `_` remains an admitted Surface inference
request in its existing positions. Metadata such as role/visibility/recursion
annotations and ordinary import lists keep their own grammar.
Postfix brackets belong to the completed expression regardless of whitespace;
expression-start brackets remain variadic frames. Invalid type entries never
retry a value-frame interpretation. Exercise nested forall types, empty-group
rejection, `_` inference, Unit, computed callees, authored consecutive type
groups and returned binders across native, Prime and editor parsers.
One written value group applies one Function layer; tuple packing is not curry
crossing. Check UFCS against its selected written stage, including outermost
reverse insertion, a final type-stage boundary, and first/last block prefixes.
Earlier block prefix groups remain separate stages; the whole final prefix
value and whole projected blocks form one right-associated product for one
stage. Exercise nested products, omission versus Unit, direct calls and
residualization after all blocks. Explicit block-call type arguments remain excluded. Whole direct macro chains
must gather public inference before source admission/action without absorbing
ordinary generated-result calls into reflected operands.

## Macro binding and highlighting

Explicit macro selectors and ordinary value introductions share one source-order
lexical ownership model. Check local/parameter/numbered-family shadowing, import
aliases, earlier same-module declarations, transparent grouped heads and native
`let`/`recur` ownership. Kind and visibility checks apply even with empty bodies.
No provider census, whole-file macro-name set, type-driven fallback or runtime
provenance may decide call classification.
Every nonempty written macro block prefix uses ordinary argument-list parentheses,
including a lone name, signed literal or Boolean literal. Zero-prefix calls
omit the list; direct empty lists supply Unit, while block-call UFCS rejects
empty lists. Preserve native `let`/`recur` ownership and ordinary nested
expression grouping. Explicit list boundaries determine prefix ownership.

Rich tooling refines known macro bindings to the macro token category; standalone
TextMate and Tree-sitter consumers may retain generic callable coloring. Exercise
actual REPL scope, Kiodoc assembled/harness scope and display-span mapping,
module-end doc-comment examples, desktop/browser providers, and edits retracting
an earlier macro classification. Shared classifier tests do not substitute for
each provider boundary.

## Fixed operator kinds and delayed operands

A fixed operator's complete public grammar includes function/macro kind,
slot extent and delay markers. Parser, formatter, imports/aliases, REPL
selectors, LSP symbol/type displays and cache dependency discovery retain
those distinctions; conflict checking still uses the existing body-independent
dispatch key. The dot wraps the whole operand once before ordinary call
preparation and does not consult a macro's trailing descriptors. Preserve
comments around marker tokens when canonicalizing their spacing.

Keep ordinary declarations named `op` distinct from `op_macro` declarations,
including incomplete editor text and lazy item splitting. Macro operator
targets carry the actually resolved macro identity into ordinary expansion;
user spelling and generated names confer no primitive authority. Exercise
alias/provider edits and same-name function/macro controls as well as
ordinary runtime effects and explicit nested-thunk operands.

## Impact checklist

For literal inference changes, separate lexical admission from carrier inference.
Exercise all four categories, explicit annotations, concrete return/argument/
binding context, connected generic-call constraints and unresolved local
boundaries. Include applied aliases, source-order scope, unrelated role-bearing
import additions and signed/numeric category/range/IEEE rules. A visible type
must never become an ambient fallback. Test checked Boolean specialization
from explicit, condition and expected-callable evidence; reject rigid generic
Boolean parameters while preserving ordinary generic result types. Fresh
Prime artifacts validate concrete literal annotations and Boolean specializations.
Source-authenticated missing-dot fixes preserve literal aliases and stale-edit
refusal; configuration words and inspector selectors remain separate grammar.
An accepted consumer-import extension preserves body grouping, identities,
types and meaning.

A surface or semantic change is not complete until the language and tooling agree on the same accepted spelling and the same user-facing behavior. Before implementation or landing, state the impact argument: which bridge surfaces must change, which are intentionally unchanged, and why "unchanged" follows from the boundary being modified.

Use that argument to choose the affected surface area:

- Parser and parse diagnostics: accepted forms, rejected old forms, recovery, and targeted suggestions where practical.
- Formatter and style contract: canonical spelling, list layout, shorthand expansion/collapse, and stable round trips.
- Lowering boundary: every surface-only form disappears before Kio'; phase-local elaboration data must be consumed before the boundary closes.
- Typechecker behavior: synthesis/checking rules, expected-type flow, open-world argument when name resolution, imports, dispatch, or inference are involved.
- Normalization behavior: `equiv`, REPL `:normalize`, macro execution, and any compile-time evaluator path that observes the changed semantics.
- Generated corpus: whether `kio-gen-rs` can still generate well-typed programs for the changed language and whether it should generate the new shape.
- Embedded and noncanonical source: include nested demo and test API modules, Kio driver modules, program templates, shell-embedded examples, and formatter fixtures excluded from ordinary formatting checks. Review matching expected-output changes against the approved syntax change, preserving each fixture's subject and comments.
- File-kind surface parity: every Kio-family file kind in the shared file-kind vocabulary must be considered together, not by a hard-coded module/package subset. Check `kio fmt`, `kio doc` `variant=KIND`, highlighters, LSP routing, IDE plugins, syntax fixtures, and docs against the whole set.
- LSP: diagnostics, formatting, semantic tokens, hover, completion, code actions (including the source they generate), definition, references, prepare-rename, and rename. File-kind routing must follow the shared Kio-family predicates rather than ad hoc suffix lists.
- REPL: command parsing, scope/output, expression queries, completion, highlighting, and multiline input; distinguish the shared inspector core, terminal front end, and browser wrapper.
- Editor grammars and extensions: tree-sitter, TextMate, the VS Code extension's bundled grammar/configuration, fixtures, and highlight corpus coverage; keep them consistent with `kio debug tokens` and cover every Kio-family file kind.
- Contract-surface changelog: `kio sig` records the host/bridge contract surface — a change to the host boundary, exports, or the types they reach moves what a `*.sig.kio` records; see [`specs/versioning.md`](../../specs/versioning.md).
- Public contracts: update `specs/grammar.md`, `specs/language.md`, `specs/prime.md` when the grammar or lowering boundary changes; update `specs/style.md` for formatter behavior and `specs/cli.md` for LSP/editor behavior.
- Public docs: update affected guides, tutorials, and runnable snippets in `docs/`.
- Tests: add focused parser/formatter/type/normalizer/LSP/highlight/VS Code coverage plus success and negative goldens for the new behavior and any rejected old behavior.
- Agent guidance and audits: assess affected authoring instructions, workflows
  and checking assumptions using
  [`review-commits`' coherence check](../skills/review-commits/SKILL.md#instruction-and-audit-coherence).

For syntax touching row forms, labels, field access/update, imports, dispatch, or inference, state the open-world argument explicitly before implementing the rule.

## Canonical formatting, not spelling preservation

The formatter chooses the canonical form in `specs/style.md`; it is not a
source-spelling round-tripper. Do not retain redundant syntax merely because
the author wrote it or because a parser flag remembers it. Distinguish accepted
input from canonical output: alternative spellings covered by a normalization
rule must converge to its chosen spelling. A missing or conflicting substantive
canonicalization rule needs a contract decision, not a preservation test that
silently settles the policy; this does not make unspecified layout details
approval-gated.

Canonicalization preserves meaning, comment text and ownership. Keep syntax
that carries parsing, binding, typing, evaluation, or documentation meaning,
and respect explicitly specified spelling and layout distinctions, including
the style contract's blank-line and comment-paragraph preservation. Do not infer that all
semantically equivalent programs must print identically or erase annotations
whose redundancy would require typechecking to establish. When shortening a
construct, account for trivia on the removed tokens and children; a shorter
result is not correct if it drops a comment.

Comment-conservation tests must exercise original input before canonicalization
and legal boundaries inside constructs, including tokens removed by shortening.
Injecting only into already formatted lines misses those ownership paths.

Test each normalization with deliberately different accepted inputs and the
same exact expected output, plus load-bearing and comment-bearing controls.
Also check reparsing and idempotence. Neither idempotence nor formatting the
existing corpus proves that the canonical choice is right. For rewrites that
erase or regroup syntax, verify the affected meaning before and after through
appropriate parser, type/signature, or execution assertions. Audit tests that
preserve source spelling against the contract, rather than treating existing
expectations as authority. Reuse these checks at the affected CLI, LSP and
Kiodoc boundaries; keep published-example coverage explicit.

## Actionable diagnostics and native fixes

When adding or changing a diagnostic that proposes a source correction, assess
whether it can carry a structured compiler-produced fix through the existing
LSP code-action path. Record the available fix and its evidence, or explain why
an edit needs an author choice or cannot preserve source ownership and intent.
A help line alone does not establish that an editor action exists; not every
diagnostic needs an action.

Build edits from authenticated syntax and resolved facts, never diagnostic
prose. Preserve comments, formatting and unrelated source, retain the current
document/provider checks, and reanalyse the edited program. Do not invent a
capability or resolve ambiguous intent just to offer a fix. Mark scaffolding or
potentially intent-changing edits with the appropriate applicability rather
than treating them as automatic repairs.

Test the structured edit, actual LSP discovery and application, the resulting
diagnostics, and relevant stale or invalid-repair refusals. Editor presentation
claims additionally need the real editor provider with other extensions
disabled; distinguish a native edit from an AI action. A diagnostic golden or
an edit-string assertion alone does not prove editor application works.

## Completion currency

Inspection and completion present resolved declaration kinds and function
purity alongside the signature or type. Use authenticated declaration facts;
function types have no purity class, and a macro's compile-time purity does
not classify its generated runtime term. Keep ordinary values' inferred types
visible without guessing declaration metadata from their displayed spelling.

Completion is bidirectional: identifier candidates equal the bindings in the cursor's lexical/source-order scope and selected namespace, with shadowing and ordinary import visibility preserved. At every position where the next user-selectable named atom comes from a closed language-defined vocabulary, contextual completion equals the choices that can still lead to an accepted, canonically spelled construct. This includes all five Kio-family file kinds, not only modules and packages. Reserved, removed, rejected, duplicate, mutually exclusive, order-invalid, and context-invalid choices are not suggestions. Parser recognition for a targeted diagnostic does not make a choice eligible; derive eligibility from the effective grammar, semantics, package/versioning and target-key contracts.

Trace the shared context/scope machinery through each actual LSP, terminal REPL, and browser/wasm completion entry point. Suppress unrelated identifier suggestions in comments, literals, and binder-introduction positions; distinguish language-name matching from command discovery. Replacement ranges cover the complete started identifier or operator run. Candidate labels and optional type/documentation enrichment must retain the exact selected binding, provider edge, and current source version. Omit unauthenticated enrichment rather than attaching a same-spelled or stale declaration's metadata. Bounded menu presentation does not authorize discarding eligible matches; apply the response and menu contracts in `specs/cli.md` separately.

REPL declaration queries use the single-import-item selector contract in
`specs/cli.md` § Declaration selectors. Keep operator grammar and provider
identity separate from the callable implementing it, and replace the whole
selector in completion. Retired braced label selectors are not candidates. Qualified
operator insertion includes its required outer parentheses; inner import-pattern
parentheses are independent. Shared declaration rendering does not imply that
Kiodoc directives or expression queries accept REPL selector syntax.

Named selective aliases retain separate source-selector and local-binding
identities. Completion and unused-import analysis follow the local destination;
definition and declaration metadata follow the selected provider declaration.
LSP local-alias references and rename exclude the source selector, while
provider rename preserves explicit destinations, including `name as name`.
REPL `:refs` remains declaration-oriented across all selected local spellings.
Keep alias occurrence facts source-authenticated and editor-local; do not
replace the semantic provider identity or overwrite its declaration site.
Exercise shadowing in function bodies and type declarations, both full and
focused analysis, and source-only declarations erased before typechecking.

An authorized change to eligibility moves the affected providers and focused evidence together: exact positive/negative candidate sets, insertion into an otherwise-valid continuation, and edit-driven retraction. Cover document/provider edits, shadowing, incomplete intermediates, and close/reopen or session reload. Shared collector tests do not substitute for the distinct JSON-RPC, interactive terminal, and browser completion boundaries. Static audits inspect implementation and existing evidence; execution follows the scoped validation procedure below when requested or required by an authorized fix.

## Incomplete-source recovery

When an unfinished declaration can be isolated, preserve subsequent complete
declarations for editor navigation and completion. Recovery follows written
structure, not provider contents or successful typechecking, and never grants
an incomplete module semantic acceptance. Check valid multiline continuations
before choosing a resynchronization boundary.

Use deterministic edit-and-restore traces with expected surviving declarations
and useful diagnostic ranges. Fresh/incremental parse agreement supplements
those assertions; two equally unhelpful trees are not successful recovery.
Tree-sitter incomplete forms retain explicit recovery nodes or ERROR/MISSING
state, while complete controls contain none. Bound scanner work on long comment
and delimiter runs, and exercise the actual LSP symbol/completion requests.

## Focused tooling validation

Before the expected-green broad gate, run the affected executable tooling suites
listed in [`local-ci.md` § Language and editor-tooling preflight](local-ci.md#language-and-editor-tooling-preflight).
Library tests, parser goldens, lexical agreement, and real LSP/editor integration
tests exercise different boundaries; one does not substitute for another.
Include embedded Kio in Rust/JS/TS test fixtures and source-producing features,
not only `.kio` files. A syntax migration must preserve each fixture's actual
subject. Apply and reanalyse a code action's edit, checking the intended syntax
and semantic result while preserving any deliberately remaining diagnostics.

Editor lifecycle changes also need failure-injection coverage at the real client
boundary: missing or invalid compiler selection, startup and connection failures,
bounded automatic restart, settings/manual recovery, and deactivation during
startup. Distinguish a closed transport from an exited process; assert child
cleanup and absence of unhandled failures, not only a successful test-run exit.
Recovery evidence includes diagnostics from the actual compiler. Keep desktop
and remote Node hosts distinct from deliberate browser-only highlighting, and
verify status and notification behavior without treating ordinary source errors
or version-number differences as availability failures.

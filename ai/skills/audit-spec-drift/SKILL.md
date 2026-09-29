---
name: audit-spec-drift
description: Find behavior compiler and backend implementations have but the specs don't (or vice versa) — covers cli.md, exit-codes.md, grammar.md, package.md, style.md, language.md, prime.md, backends/*.md
allowed-tools: Read, Grep, Glob, Bash
---

# Spec drift audit

`specs/` is the contract. ai/topics/specs.md is explicit: "Specified behavior is a contract; don't break it inadvertently." This skill finds places where the implementation has drifted from the spec — in either direction. Every implementation of a backend that exists in the audited tree must conform independently; see § 7.

Read AGENTS.md § Universal rules — Backend decisions propagate by semantic
applicability, ai/topics/specs.md and the index it links, and
[`ai/topics/emit.md`](../../topics/emit.md) § Decision record and propagation
gate before starting.

## 1. CLI surface

Compare the `kio` binary's argument handling — `kio-rs/src/main.rs`, dispatching through the hand-rolled `kio_lang::run` in `kio-rs/src/lib.rs` (there is no clap) — against `specs/cli.md`:

- Every subcommand and flag listed in `cli.md` must exist in the binary with matching semantics.
- Every public flag in the binary must appear in `cli.md`. The exception is *internal/debug* flags (e.g. `kio debug tokens`), which AGENTS.md § Universal rules — Debug-only surfaces are internal keeps out of public `specs/` — those should NOT be in `cli.md`. Confirm each non-spec'd flag has the "internal" comment.
- The `kio-prime` binary is internal — it must not be documented in `cli.md`.

## 2. Exit codes

Compare `specs/exit-codes.md` against the actual exit codes the binaries return. Find every `process::exit`, `ExitCode::from(...)`, and error → exit-code mapping in kio-rs, and verify each category lands on the documented code. Tests that assert exit codes (`ci/checks/per-case/`, `ci/run-tests.sh`) should use the same codes.

## 3. Grammar

`specs/grammar.md` carries the canonical productions in three layers (Kio' / Kio surface / package files). Compare against the kio-rs parser (`kio-rs/src/parser*`, `lexer*`):

- Every production in `grammar.md` should be parseable.
- Productions the parser accepts should appear in `grammar.md`.
- The Kio' subset (productions tagged Kio') should match what `prime::lower` accepts.
- Identifier character classes and naming roles must agree across the canonical
  `IDENT` production, `language.md` / `prime.md`, the full and independent
  Prime lexers, and editor classifiers. In particular, do not let prose claim
  Unicode identifier characters while the grammar and consumers admit only
  ASCII.

This isn't a line-by-line diff — focus on shape: missing productions, ambiguous-looking productions, productions that admit syntax the parser rejects (or vice versa).

## 4. Package files

`specs/package.md` defines `<pkg>.pkg.kio` and the build target blocks. Compare against kio-rs's package handling (`kio-rs/src/package*`):

- Every required field listed in the spec is required in code.
- Every field code reads is documented in the spec.
- Per-target build-block keys live in `package.md` § Build target files, *not* in per-backend pages.

## 5. Style

`specs/style.md` describes the canonical style `kio fmt` produces. Compare against `kio-rs/src/format*` (or wherever the formatter lives):

- A1 leading-comma layout, indent rules, `use`-block ordering, literal canonicalization.
- If the formatter has knobs the spec doesn't mention, flag it.
- Enumerate every Kio-family file kind from `kio-rs/src/file_kind.rs` and `KioFileKind` in `kio-rs/src/ast.rs`. Each kind should either have formatter routing plus style/spec coverage, or a spec-stated reason formatting is irrelevant. Hard-coded module/package-only lists in `kio fmt`, LSP formatting, highlighters, IDE plugins, Kiodoc variants, or docs are drift unless the omitted kind is outside that surface by construction.

## 6. Surface and Prime semantics

`specs/language.md`, `specs/prime.md`, and `specs/formal/elaboration.md` describe what typechecks and what doesn't. Check the typer (`typecheck_core`, `typecheck_full`, `prime::typer`) against:

- Each typing rule named in the spec.
- The bidirectional discipline pinned by `specs/formal/elaboration.md`: source-bounded exact type-argument inference, shallow skolemization, positional type-args, no implicit generalization, no backward type flow from later uses, `let` does not narrow the RHS (explicit binders make defaulting-to-monomorphisation unnecessary). An inferred whole type-argument goal may copy one complete polytype already exposed by finite connected application evidence, but the typer must never invent or generalize a `forall`, guess binder placement, leave inference goals beneath a `forall`, search ambient declarations, backtrack, or admit a polymorphic value into a proper subterm of a declared monomorphic slot. Verify that this is the ordinary call rule rather than tuple / `__pair__` / named-producer special authority, and that lowering materializes the inferred type argument because Kio' still requires it explicitly.
- **The one-shot local discipline** (the **One-shot local discipline** contract bullet and § 10's **Finite owner-scoped application domains** in `specs/formal/elaboration.md`): the elaborator remains syntax-directed, but one finite inference domain may connect application layers and unfinished nested applications from the same written call tree. Every equality goal is source-bounded and owner-scoped; independently completed nested calls contribute only closed results; nothing crosses a semicolon or escapes its lexical owner. Each source premise is entered once and each retained source action is consumed at most once. Each application owner carries at most one pending expected-result equation; a connected tree may contain finitely many, one per owner. A pending equation may make an initial blocked attempt and is discharged once after the descendants relevant to its owner close. Flag derivation replay, backtracking, fixed-point iteration, worklists, global constraint stores, owner escape, or inference across a semicolon. Do not flag a finite connected domain merely because it crosses an application-node boundary, and do not infer an asymptotic bound: substitution and rank-N comparison can be super-linear. Watch changes to the application lifecycle in `typecheck_core/apply.rs`, `apply/frontier.rs`, and `goals.rs` for both illicit global solving and accidental re-execution.
- **Written application authority.** Check ordinary, intrinsic, host, UFCS, and user-elaborator calls against the same resolved-type rule. A syntactically empty direct/prefix surface call omits leading type arguments and contributes one written Unit value; a nonempty packet containing only type arguments or `_` placeholders remains residual after substitution, even when the instantiated or alias-unfolded domain is Unit. UFCS inserts its receiver before application checking: a bare `r.>f` plans as `f(r)`, a written-empty `r.>f()` is rejected before its presence can be erased, and `r.>f(())` plans as `f(r, ())`. Require the same omission/empty/explicit-Unit distinction in all four directions for ordinary, member, and bang callees. In particular, for `nil : [A] . -> List(A)`, `nil(A)` is residual while `nil(A)()`, `nil(A, ())`, and `().>nil(A)` saturate; the UFCS form saturates because its receiver is Unit. Direct `nil()` needs local constraints that solve `A` and lowers with both arguments explicit. Flag any use of declaration parameter groups, `Signature` / `HostFn` ABI fields, callee identity or provenance, or an elaborator's private ABI to change completion, grouping, residualization, or UFCS placement. Kio' must carry the selected type and value applications explicitly, in observable order; the independent verifier must not reconstruct them from producer history.
- The elaborator bang-call mechanism (`specs/language.md` § Elaborators are imported, not ambient): `IDENT '!'` resolving to an imported `elab` declaration, checked against its call type, recorded in the `Elaborations` side channel, and substituted at the Lowered → Prime boundary. The reference coercion palettes (algebraic `iso!`/`into!`/`onto!`/`align!`/`ease!`/`atom!`, spine `fit!`/`reorder_*`/`narrow_*`/`widen_*`/`flatten_*`/`one_*`) plus `match!`/`derive!` are user-defined libraries in `test-data/poc/elab/`; their per-form rules are documented as case studies in `docs/poc/elab.md` and `docs/poc/optics.md`, not in `specs/`. Drift to check there is between those POC `.kio` sources, the elaborator implementations the compiler ships, and the case-study prose.
- **No user-declaration privilege by spelling or distinguished library identity** (AGENTS.md § Universal rules — User declarations never become compiler primitives by name). Sweep semantic branches in parsing, desugaring, recursive lowering, structural recovery (including `recover_to_low`), capability annotation, typing, substitution, evaluation, optimization, and every backend for hard-coded user-spellable names, constants that expand to shipped library names, or comparisons against compiler-hard-coded or otherwise distinguished ordinary-library identities. A branch that changes typing, tail-position status, argument interpretation, lowering, visibility, capabilities, or evaluation is a finding; changing the key from `"match"` to a hard-coded fully qualified `match` declaration does not cure it. Generic use of the identity selected by the resolved program is legitimate for exact capture/visibility-edge validation, nominal equality, ordinary dispatch, host linking/backend naming, diagnostics, and caching. Raw argument indices are admissible for a grammar-defined form, but not as a remembered user-library ABI in place of resolved semantic slots. Reserved intrinsics are exempt only when their compiler semantics are explicit in the specification. Also flag spec prose that grants compiler-only behavior to one Kio-authored library declaration without defining an explicit general language construct or capability. Require a same-spelling unrelated declaration as a negative control, a differently named or rehomed structurally equivalent declaration as a positive control for general behavior (or a renamed declaration carrying the authorized explicit capability), and every applicable qualified/selective and prefix/UFCS route.
- The Kio'-subset rules in `prime.md` — Kio' is Church-style System F; the surface is a bidirectional elaborator over it.

This is broad — focus on recent typer changes (`git log --since="3 months ago" -- kio-rs/src/pass/typecheck_core/ kio-rs/src/pass/typecheck_full.rs`) and check whether each commit has a corresponding spec update.

## 7. Per-backend specs

Start from every normative shared law in `specs/backends/README.md`, including
every `**Applies when:**` clause. Restate its semantic predicate, enumerate
every shipping backend and semantically distinct occurrence, and compare each
applicable implementation against the shared law. Then check that the
per-backend page references the shared heading and records only its host
realization or concrete caveat. A law left only in one backend page or emitter,
a family-inferred result without backend evidence, and an applicable peer left
unassessed are drift findings.

For each `specs/backends/<lang>.md`, discover every implementation of that
backend in the audited tree and check each one implements what the page
documents. A shipping backend is presumed to implement the full Kio contract
and carries no independent backend version or maturity tier. Its required Host
API stability field governs compatibility only: `evolving` does not authorize
implementation drift or a break absent explicit approval, while `stable`
prohibits compatibility breaks. Run
`ci/checks/repo-lint/backend-api-stability.sh` for exact field/value/mirror,
default, rollout boundary, rename/removal, and transition-record checks.
Spec-omits-impl-has is a finding when the omitted behavior is host-observable
contract surface; internal helper factoring and other codegen details that
never cross the host boundary remain outside the per-backend spec.
Spec-has-impl-doesn't is always a finding. A concrete known-caveat banner
reports a defect or host-source incompatibility; it does not authorize a
fixable implementation gap as contract.

**Opaque host-type identity is not host-representation injectivity.** Check
that one declaration-keyed binding entry exists where the backend exposes
typed bindings, but do not require different Kio `host type` declarations to
select different concrete host types or type constructors. Flag an emitter or
backend spec that invents wrappers, brands, tags, defined types, `NewType`s,
or markers solely to preserve the declarations' distinct Kio identities in
the host representation. Explicit Kio `newtype` representation and
independently justified target representations are separate from this check.
Do not apply the wrapper ban to an independently authorized,
declaration-scoped adapter for a relationship the host cannot state; audit
that adapter against its own narrow scope and failure or unsafe invariant.
This conditional does not authorize or claim that such an adapter exists.

**Every implementation of one backend conforms.** Discover the backend
implementations present in the audited tree rather than assuming a fixed
implementation inventory. Every implementation independently respects
`specs/backends/<lang>.md`; the spec is the single contract, not whichever
emitter was inspected first. When more than one exists, behavior present in
only one is drift in another implementation or in the spec. Conformant
implementations need not emit identical source: naming, statement ordering,
helper factoring, and whitespace may differ while the specified value
representation, host-call ABI, calling convention, and runtime behavior agree.
A byte-for-byte cross-implementation requirement is itself a finding.

**Retained signature-history provenance.** For every backend that consumes
sealed `*.sig.kio` history, inventory the complete emitted host-visible
retained closure, not only the removed root. Flag any history-only method,
binding, carrier, alias, shell, constructor, or helper without the target's recognized
deprecation marker; any method implementation or type/generic selection a
current host must provide because of history; and any history-only entry in
loader matching, current-host validation, a runtime adapter, capability,
dispatch, or execution plan. Also flag the converse: an exact declaration
reached live must not become deprecated merely because history reaches it too.
Also construct incompatible same-identity declaration epochs: live must win,
and every retained root and dependency that cannot keep its exact frozen
meaning must be omitted rather than merged, widened, or chosen by replay order.
Require matching concrete source-edit caveats for that omission. Where
optional retention is impossible, require omission plus the ordinary
mutual-cited concrete host-source caveat.

**Degraded-support cross-reference.** A code comment in `kio-rs/src/backends/<lang>*` saying a spec-admitted feature, public member, or boundary shape is degraded, unsupported, omitted, dropped, substituted, or replaced by a placeholder must be mutually anchored to a spec limitation or known-broken-behavior section:

- Grep the emitter case-insensitively for the suspect-phrase set (`degenerate`, `degenerated`, `omit`, `omits`, `omitted`, `omitting`, `omission`, `drop`, `drops`, `dropped`, `dropping`, `unsupported`, "not supported", "substitute … with `()`", "for shadowing", "not for actual invocation", "placeholder for", "we simplify by"). Classify each hit by its subject before checking citations: `omit` / `drop` is a limitation signal only when the emitter withholds host-observable, spec-admitted behavior, not when it removes redundant syntax, an internal helper, or another representation detail that leaves the contract intact.
- `Carve-out` / `Carve-outs` is not a limitation signal by itself. Backend specs use `### Carve-outs` for ordinary conforming FFI representations, including unit and function values. Do not enumerate such a section unless its substance separately says that admitted behavior is unavailable, unsupported, degraded, omitted, or dropped.
- A code comment with no spec anchor — emitter degrades, spec silent — is drift (the user has no way to discover the limitation from the contract).
- A spec limitation with no corresponding code comment — spec says "feature X is degraded on this backend" but the emitter has no comment at the degradation site — is also drift (a future change to the emitter could silently break the spec'd contract).

Both directions are findings. The fix is symmetric: name the spec section in the code comment, name the file/site in the spec disclaimer, and mirror the concrete caveat in the top banners of the backend spec and host guide.

**Per-backend contract-limitation discharge (report-only).** The publication-header and mutual-cite checks above verify that a limitation is visible in the spec, host guide, and emitter — they do **not** verify that the impossibility *claim* is true. No audit can decide that mechanically: whether a backend genuinely cannot express a spec-admitted shape is a semantic judgment. The signal-based sweeps elsewhere leave this gap wide open — [`audit-partial-implementations`](../audit-partial-implementations/SKILL.md) § 6 explicitly **excludes** arms terminating in `unreachable!` / `panic!` / span-bearing `Err(...)` (its § 6 list, "arms terminating in … these are proven-invariant paths or surfaced errors, not silent gaps"), and its § 2 only classifies the *shape* of such arms (panic vs. user-error), never whether the impossibility holds. Consider an emitter `EmitError` / `Err` / `unreachable!` arm over a spec-admitted shape with all three publication sites present: a detailed limitation and concrete top banner in `specs/backends/<lang>.md`, a matching top banner in `docs/hosts/<lang>.md`, and a comment at the exact emitter degradation site citing the detailed spec section. Those sites can pass every mechanical publication and signal check while the impossibility claim is still false. If the host's escape hatch can express the shape, the manual discharge below must still return `fixable` and report a finding.

This check closes that gap by forcing a manual discharge per claimed contract limitation. It cannot auto-verify — it **lists** and **demands a decision**:

1. **Enumerate every claimed per-backend contract limitation**, from the implementation and publication surfaces:
   - Each emitter arm — `EmitError` / `EmitError::unsupported` / `Err(...)` / `unreachable!` / `panic!` / `unimplemented!()` — taken over a shape the spec *admits* on that backend (`kio-rs/src/backends/<lang>*`). An arm guarding a genuinely out-of-spec input is not a contract limitation; one rejecting a spec-admitted shape is.
   - Each `specs/backends/<lang>.md` paragraph that substantively claims a per-backend limitation or says admitted behavior is known broken, unavailable, unsupported, degraded, omitted, or dropped, plus the required matching top banners in that spec and `docs/hosts/<lang>.md`. Do not include a `Carve-out` / `Carve-outs` heading or representation note unless it makes one of those substantive claims.
2. **For each, force the discharge question:** *is this GENUINELY impossible — the host's escape hatch is exhausted — or is it fixable work presented as a limitation?* The escape-hatch standard is [`ai/topics/emit.md`](../../topics/emit.md) § The escape hatch: a shortcoming is genuinely impossible only when the host language has **no** escape hatch (no `unsafe`, no FFI, no unchecked cast — e.g. Elm). Where an escape hatch exists, the default is to fix it with the safe / idiomatic construct; "the safe path is hard" is not "impossible." A safe fix that was simply not yet attempted is a partial implementation (cross-ref [`audit-partial-implementations`](../audit-partial-implementations/SKILL.md) § 6), not a limitation.
3. **Report each claimed limitation with its discharge verdict** — `genuine` (escape hatch exhausted or absent; both banners and the exact emitter citation are present) or `fixable` (a safe path exists that wasn't taken). A `fixable` verdict is a finding even though all three sites are intact, because the limitation is false. This step is **report-only**: it lists the claimed limitations and records the manual verdict; it does not auto-verify impossibility and cannot pass/fail on the claim itself.

## 8. Recent commits

Run `git log --since="3 months ago" --stat -- kio-rs/src/` and look for commits that touch implementation files matching a spec topic (CLI, parser, typer, format, backend) *without* touching the relevant `specs/*.md`. Each such commit is a candidate for drift.

## How to report

Group findings into:

1. **Spec says X, impl does Y** — break of contract; user-visible.
2. **Impl does X, spec is silent** — undocumented behavior the user may come to rely on.
3. **Drift candidates** — recent commits without paired spec changes.
4. **Internal flags missing the no-impl-in-spec marker** — debug knobs that exist in code without the explanatory comment.
5. **Per-backend contract-limitation discharge (report-only)** — each claimed limitation (an emitter `Err` / `unreachable!` arm over a spec-admitted shape, or spec prose saying admitted behavior is unavailable / unsupported / degraded / omitted / dropped), its mirrored spec/host-guide banners, its `genuine` / `fixable` verdict per § 7, and for `fixable` verdicts the safe path that was not taken.
6. **Backend implementation conformance** — for every implementation found,
   behavior that drifts from the shared and per-backend specs, including
   behavior present in one implementation but not another, plus any
   byte-for-byte cross-emitter requirement that should instead be
   FFI-contract/behavioral equivalence (§ 7).

For each finding, cite spec section and code file/line.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

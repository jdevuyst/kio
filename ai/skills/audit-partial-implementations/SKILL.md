---
name: audit-partial-implementations
description: Sweep for residual-behavior comments, TODO carve-outs, "fixed" claims masking partial slices, and compile-time evaluator residuals left stuck on reducible terms
allowed-tools: Read, Grep, Glob, Bash
---

# Partial-implementation audit

AGENTS.md § Universal rules — No partial implementations forbids closing an issue with a slice that handles the easy case and leaves residual behavior. This skill sweeps for the symptoms: comments that paper over a gap, claimed-fixed issues with carve-outs, and "for now" smells.

Read AGENTS.md § Universal rules — No partial implementations before starting.

## 1. Suspect comment phrases

Grep `kio-rs/src/`, `specs/`, `docs/`, `test-data/poc/`, and other tracked source for phrasing that smells like a deferred case. (POCs are user-facing reference modules; scaffolding-shaped comments there are particularly load-bearing findings since the module's header claims drop-in readiness.)

- `TODO`, `FIXME`, `XXX`, `HACK`, `BUG`
- "for now", "for the moment", "currently", "still", "today"
- "residual", "stays as", "left as", "remains as"
- "not yet", "doesn't yet", "will be", "to be handled"
- "punt", "defer", "deferred"
- "open question", "tbd", "TBD"
- "are spelled as", "is spelled as" — surface-form carve-outs that mask a missing dedicated syntax
- "workaround", "work around", "hack to" — shorthand for "we haven't fixed the root cause"
- "isn't supported", "isn't yet supported", "not supported", "unsupported"
- "placeholder", "stub", "unimplemented", `unimplemented!`
- "degenerate", "degenerated", "degeneration" — especially the active-voice "we degenerate by …" pattern that documents a per-backend carve-out as a deliberate simplification
- "for shadowing", "for testing", "for verification only" — feature exists only for non-functional purposes (so the implementation skips the real work)
- "not for actual invocation", "not for actual use" — same shape, paired with a justification ("the corpus declares these for X only, not Y")
- "future use", "later slices", "ride along", "populated by" — typical phrasing for unused symbols carried as scaffolding
- `allow(dead_code)` — the lint suppression itself, when on hand-written items

For each hit, read the surrounding code and classify:

- **Genuine deferral with a clear next step** — fine if scoped explicitly to a follow-up slice or scratchpad note. AGENTS.md allows this when the slice is honestly incremental.
- **Paper-over of a partial fix** — a code comment documenting a gap behind a "fixed" claim. Flag.
- **Stale annotation** — the underlying gap has since been filled but the comment lingers. Flag for removal.

## 2. Spec / impl drift hiding behind carve-outs

AGENTS.md § Universal rules — Bugs vs. input errors carves the line: known-unreachable paths should panic with a message naming the upstream contract; *user* errors return a structured `Error` with a span. The audit checks both sides — a carve-out comment without spec backing, and an error return that's actually masking a known-unreachable path.

Walk through:

- Each `unreachable!`, `panic!`, or `Error::Internal` in kio-rs and confirm the upstream contract is named in the message or an adjacent comment.
- Each match arm that returns an error with phrasing like "not supported", "unhandled", "for X targets only" — confirm the limitation is stated in the relevant spec section.
- Each `Error::Type` / `Error::Internal` / structured `Error` return from a path the surrounding code or comments describe as believed-unreachable. If a comment says "this can't happen because …" but the code returns a user-shaped `Error::Type` ("expected X, got Y"), that's the puzzlement AGENTS.md warns about — the user sees a type error whose real story is that an upstream invariant was violated. Such returns should be `unreachable!("…")` naming the invariant, not error returns. Flag.

A carve-out comment without spec backing is a finding. A user-shaped error return from a known-unreachable path is a separate finding type — the bug is the *shape* of the error, not the absence of a fix.

## 3. Issue-tracker cross-check

If the user supplies issue numbers (or `gh issue list --state closed --limit 50` is available), spot-check recent claimed-fixed issues:

- Find the closing commit (`git log --all --grep "#<num>"`).
- Read the diff and the issue body.
- Confirm every sub-case in the issue body is handled.

A closed issue whose closing commit handles only some sub-cases is the primary finding type for this audit.

## 4. Surface-form removal coverage

The no-partial-implementations rule (AGENTS.md § Universal rules — No partial implementations) applies pass-by-pass to the front-end: every pass must handle the full input language it claims to accept; partial coverage with carve-outs is the smell this section sweeps. For each pass in kio-rs (`desugar`, `label_elab`, `prime::lower`, `typecheck_full`, `substitute`), check whether it claims to accept the full surface language and whether any arm returns "unsupported" / `unimplemented!()` / silently skips. Flag asymmetries between claimed-accepted and actually-handled inputs.

## 5. Dead-code suppressions in hand-written code

[`ai/topics/implementation.md`](../../topics/implementation.md) § No `allow(dead_code)` in hand-written code forbids `#![allow(dead_code)]` and `#[allow(dead_code)]` on hand-written items under `kio-rs/` and `test-data/`. The attribute is the canonical scaffolding tell: it documents that a symbol has no user yet but suppresses the compiler signal that would say so.

The grep:

```
grep -rn 'allow(dead_code)' kio-rs/src/
```

For each hit, classify:

- **String literal or `push_str` arg in `kio-rs/src/backends/rust/emit.rs` or `kio-rs/src/backends/rust/runtime.rs`** — the carve-out: the Rust backend stamps `allow(dead_code)` into emitted package code so hosts that don't exercise every runtime helper stay warning-free. Not a finding.
- **Anywhere else** — a hand-written attribute. Read the surrounding code and propose one of: delete the symbol, replace the attribute with `#[cfg(test)]`, or replace with `#[cfg(feature = "…")]`. Flag.

To filter to hand-written hits in one command:

```
grep -rn 'allow(dead_code)' kio-rs/src/ | grep -v 'push_str.*"' | grep -v '"[^"]*allow(dead_code)'
```

A non-empty result is a finding.

## 6. Silent guard / catch-all fallthrough over a constructible shape

A generated trapping default is exempt from the stub finding only when it is
history-only, explicitly deprecated, genuinely optional for a current host,
and absent from every loader/runtime/package execution route. A trap used for
a live item or a retained requirement without a real default is a finding.

The sweeps above are all signal-based: they key off a comment phrase (§ 1), an error return or carve-out (§ 2), an `unimplemented!()` or "unsupported" arm (§ 4), or an `allow(dead_code)` attribute (§ 5). They all miss the **silent** gap — a guard that narrows on a property of a spec-admitted shape and routes the complementary shape to an identity / catch-all arm, with **no** comment and **no** error. Nothing in the emitter says "unsupported", so none of the phrase greps fire; the shape is simply identity-passed and the bug ships undetected. This section sweeps for that shape.

The mechanism: in each backend emitter (`kio-rs/src/backends/<lang>/emit.rs`), find guard-chains whose predicate narrows on a shape property and then terminate in an identity / pass-through / catch-all arm. The grep tells:

- predicates of the form `is_*_eligible_*(...)`;
- explicit arity / length tests — `.len() == N`, `.len() != N`, `.is_empty()`.

Grep **both** the `match`-arm catch-all form and the `else`-branch pass-through form — the motivating gap took the `else`-branch form, so do not grep only `_ =>`:

```
grep -rnE '_ =>\s*[a-z_]+\.to_owned\(\)|_ =>\s*[a-z_]+\b' kio-rs/src/backends/
grep -rnE 'else\s*\{\s*[a-z_]+\.to_owned\(\)\s*\}|else\s*\{\s*[a-z_]+\s*\}' kio-rs/src/backends/
```

**This section is report-only.** It cannot auto-flag, because it cannot mechanically distinguish a *correct* identity arm (e.g. a Unit / Bottom shape that genuinely passes through unchanged) from a *gap* identity arm (one whose excluded shape needs type-system and call-site reasoning to rule out). It lists candidates and forces a manual discharge of each:

1. **Identify the complementary shape the guard excludes** — what does the predicate route to the identity / catch-all arm rather than the narrow arm?
2. **Decide whether that excluded shape is constructible in surface Kio *and* spec/IR-admitted** — trace the call sites and the type system, not a grep. A shape no surface program can build, and that the IR cannot carry, is not a gap.
3. **Require the excluded-constructible shape be backed by either** a cross-implementation golden that actually runs it on that backend through a fixed ordinary runner protocol, **or** a mutual-cited genuine-impossibility carve-out (per AGENTS.md § Universal rules — Per-backend limitations: only the genuinely impossible, mutual-cited). A backend-first emission may supplement this with an independently authored public-host check or a durable artifact fact, but it never counts as the runtime covering golden.

A guard excluding a constructible, spec/IR-admitted shape and silently routing it to an identity arm — with neither a covering golden nor a mutual-cited carve-out — is a **finding**, classified as a partial implementation, not a limitation. The missing golden is itself the bug; write it.

Keep evidence at the boundary it proves. Golden-owned code does not read, copy,
grep, patch, import, or native-compile generated host-backend files; it may pass
an output directory opaquely to the fixed runner. A specified Kio' artifact is
the explicit phase exception when its validation/reload/dynamic-load boundary
is the subject, and harness-owned phase checks remain allowed. Public generated
Public ABI evidence belongs first in a fixed independent runner protocol and
cross-implementation golden. Only a backend-specific host fact that runner
cannot naturally demonstrate belongs in a `HOST_INTERFACE` emission; durable
spec- or recorded-measurement-backed filesystem facts belong in
`ARTIFACT_SHAPE`, and exact private no-filesystem invariants in unit or mutation
tests. An incidental private artifact assertion is not evidence for a
constructible semantic shape.

Do **not** flag:

- arms terminating in `unreachable!` / `panic!` or a span-bearing `Err(...)` — these are proven-invariant paths or surfaced errors, not silent gaps (and § 2 already governs their classification);
- identity arms that are genuinely correct over the matched shape — discharge them as correct, with the reasoning that rules the excluded shape out.

Motivating shape to keep in view: an emitter arm that guards on a structural property — say it handles only arity 1 (an `arity != 1` predicate rejecting the rest) — and silently routes the constructible arity-≥2 complement to a bare identity arm (`bound_var.to_owned()`), masked by a "the arg list is length 1 by construction" comment, with no golden exercising the arity-≥2 shape and no spec carve-out. The comment makes it read like a limitation; it is a partial implementation.

## 7. Shared evaluator completeness — `Stuck` is for irreducible terms only

The shared compile-time evaluator (`kio-rs/src/normalization.rs` and any successor shared evaluator behind `equiv`, the REPL `:normalize`, user elaborators, generated-term templates, and future `__comptime__` hooks) partial-evaluates Kio'-shaped core terms plus an explicit primitive environment to a residual normal form. Its discipline is **completeness**: it must reduce as completely as the spec admits, firing every applicable reduction rule. A `Value::Stuck` / `Value::Atom` residual is admissible **only for a genuinely-irreducible Prime term** — one to which no reduction rule applies per the contract — for an explicit compile-time hole represented in the evaluator input (`UncheckedPrime` in kio-rs), or for an explicit opaque primitive from the primitive environment. It is **never** a stand-in for an unimplemented reduction. A *reducible* term left `Stuck` — a missing β / Λ / ι / `let` / newtype-fixpoint step — is a partial implementation, and a finding.

The reduction contract is [`specs/formal/equiv.md`](../../../specs/formal/equiv.md): § 2 (Reduction rules — the small-step rules borrowed from `prime.md` plus the `__if_then_else__` literal-recognition rule), § 3 (Residual normal forms — the exhaustive grammar of what a *valid* residual is), and § 4 (Equivalence relation). The underlying redex set is [`specs/formal/prime.md`](../../../specs/formal/prime.md) § 3 (Reduction: `β-Lam`, `β-TLam`, `β-Let`, the ι-rules `ι-LeftEither` / `ι-RightEither` / `ι-Fst` / `ι-Snd` / `ι-Newtype` / `ι-IfTrue` / `ι-IfFalse`), with the genuinely-stuck shapes enumerated there (opaque host calls; `__if_then_else__` / `__absurd__` on a stuck host call; newtype projection against an opaque atom). The newtype iso-recursive wrap/unwrap step (`ι-Newtype`) is the only "fixpoint" reduction — Kio' has no `fn` self-reference, so a stuck recursive term is the `ι-Newtype` redex left unfired, not a general fixpoint gap. § 5 of `prime.md` (Strong normalization) guarantees that every closed well-typed non-stuck term *has* a finite normal form to reach, so an evaluator that gives up early is leaving reachable reductions on the table.

**Soundness framing (context, not the target).** A `Stuck` residual is **sound**: `equiv` errs toward "can't prove equal," and a residual that should have reduced makes two arms compare *unequal* and the `equiv` discharge *fail loudly* — it never silently accepts a false equivalence. So this is a **completeness** discipline, not a soundness one. The danger is the opposite of a hidden unsoundness: it is **silent incompleteness** — a missing reduction rule masquerading as "this term is irreducible," surfacing later as a spurious `equiv` failure with no obvious cause.

**This section is report-only — it cannot auto-flag.** No mechanical check can prove a term is genuinely irreducible; deciding "no rule in `equiv.md` / `prime.md` § 3 applies" is the same semantic judgment as the carve-out discharge in [`audit-spec-drift`](../audit-spec-drift/SKILL.md) § 7. The audit instead **enumerates** every residualization site in the shared evaluator and every wrapper-specific residualization point, then **forces a per-site discharge**.

Enumerate the sites:

```
grep -nE 'Value::Stuck|Value::Atom|residualize|stuck' kio-rs/src/normalization.rs
grep -rnE 'Value::Stuck|Value::Atom|residualize|stuck' kio-rs/src/ | grep -E 'eval|normalize|elaborat|template|comptime'
```

The construction sites that mint a residual rather than reduce further (e.g. the `Value::Stuck(Arc::new(other), args)` catch-all in the application path, the `__if_then_else__` fall-through arm, the unresolved-name → `Value::Atom` path, or an entry-point wrapper that converts an unsupported shape into an evaluator atom) are the candidates. For **each** site, force the discharge:

1. **Name the shape that lands here** — what callee / redex shape routes to this residual arm instead of a reduction?
2. **Decide: genuinely irreducible, explicit opaque primitive, or a missing rule?** Cross-check against `prime.md` § 3's redex list and `equiv.md` § 2–3. A residual is **genuine** only if it matches one of `equiv.md` § 3's enumerated residual normal forms / § 2.4 opaque-host shapes (an unbound atom, a host call explicitly declared opaque by the primitive environment, `__if_then_else__` / `__absurd__` over a stuck host condition, newtype projection over an opaque atom, a closure with a residualized body). A residual that lands on a redex shape `prime.md` § 3 gives a rule for — a closure applied to enough args (`β-Lam`), a type-abstraction applied to a type (`β-TLam`), a `let`, an `__either__` / `__fst__` / `__snd__` / `N.p` over a built constructor, an `__if_then_else__` over a *literal* `true` / `false` — is a **missing reduction**.
3. **A reducible-term-left-stuck is a finding** — classified as a partial implementation. The fix is to implement the reduction, not to document the residual as a "limitation."

Do **not** flag:

- residuals matching `equiv.md` § 3's normal-form grammar or § 2.4 opaque-host shapes — these are correct, and the discharge records *why* no rule applies;
- deliberate primitive opacity declared in the evaluator's explicit primitive environment (`equiv.md` § 2.4 / `prime.md` § 3.3) — opaque host calls are *defined* to be irreducible leaves, not a gap.

## How to report

Group findings into:

1. **Closed issues with residual behavior** — issue number, closing commit, the case that's still uncovered.
2. **Carve-out comments without spec backing** — file/line, the comment, the missing spec anchor.
3. **User-shaped errors on unreachable paths** — file/line, the misclassification per AGENTS.md § Universal rules — Bugs vs. input errors. These should panic with the upstream contract named, not return `Error::Type` / structured user errors.
4. **Partial passes** — pass name, the arm that drops a case the spec admits.
5. **Stale TODOs / FIXMEs** — file/line, suggested action (resolve or remove).
6. **Hand-written `allow(dead_code)`** — file/line, the symbol(s) the attribute is masking, the recommended resolution (delete / `#[cfg(test)]` / `#[cfg(feature = "…")]`).
7. **Silent guard / catch-all fallthrough** — file/line of the guard, the constructible spec/IR-admitted shape it excludes and routes to an identity arm, and the missing backing (runtime-covering golden or mutual-cited carve-out). Report-only candidates each carry the manual discharge that decided them a finding; emission-only evidence leaves the runtime gap open.
8. **Shared evaluator residual left stuck on a reducible term** — file/line of the residualization site, the entry point(s) that can reach it (`equiv`, `:normalize`, user elaborator, template, `__comptime__` hook), the redex shape that lands there, the `prime.md` § 3 rule that should have fired, and the per-site discharge verdict (`genuine-irreducible`, `explicit-comptime-hole`, `explicit-opaque-primitive`, or `missing-reduction`). A `missing-reduction` verdict is a finding; the fix is to implement the rule, not to document the residual as a limitation.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

---
name: audit-prime-grammar
description: Verify Kio' is a strict grammar subset of Kio and specs/prime.md, kio-rs, and the independent verifier agree on every admitted form
allowed-tools: Read, Grep, Glob, Bash
---

# Prime-grammar audit

Kio' is the formal core. AGENTS.md requires it to be a strict subset of the surface language; `specs/prime.md` is supposed to describe that subset as the self-hosting target and formal-semantics carrier. Four views have to agree on what counts as Kio':

1. **The full Kio pipeline** — the superset oracle: it accepts every Kio' spelling with the same meaning.
2. **`specs/prime.md` § Grammar** — the EBNF contract, constrained by the strict-subset invariant; a spec production cannot authorize a Kio'-only extension.
3. **`kio-rs/src/pass/parser/`** — the regular-module parser used by both `kio` and `kio-prime`. `kio-prime`'s `PrimePipeline` runs the standard parser and then rejects surface-only forms in `prime::lower`; the rejection set is the parser's view of "what's in Kio'."
4. **`ci/infra/kio-prime-check-rs/src/`** — the standalone syntactic verifier, the *second* Kio' parser in the tree. The redundancy is the point: it lets the corpus check act as an oracle on kio-rs rather than asking kio-rs to test itself.

The hazard this skill catches is silent split: a construct kio-rs accepts but the verifier rejects (or vice versa), a Kio'-only form the full pipeline rejects, or a spec EBNF rule that no implementation actually exercises. Per-PR, [`ci/checks/per-case/prime-marker.sh`](../../../ci/checks/per-case/prime-marker.sh) catches *acute* breakage — a marker-case that stops parsing, or an unmarked case that suddenly does — but only against the union of currently-marked cases. This audit catches the slower drift the per-PR check can't see: both directions of the strict-subset relation, agreement-on-shape, agreement-on-edge-cases, and dead spec EBNF rules.

Read [`specs/prime.md`](../../../specs/prime.md) § Grammar, ai/topics/specs.md, and ai/topics/implementation.md before starting.

## 1. Enumerate the spec EBNF

Walk `specs/prime.md` § Grammar. Compile the list of productions and their right-hand sides. Note any productions cross-linked from `specs/grammar.md` § Kio' layer — the Kio' EBNF is supposed to be shared between the two pages, so divergence between them is its own finding.

## 2. Map to the kio-rs parser

Walk `kio-rs/src/pass/parser/` (and the lexer it builds on) plus `kio-rs/src/prime/lower.rs` (the Kio' rejection pass). For each spec production:

- Locate the parser path that accepts it.
- Confirm the ordinary full Kio pipeline accepts the same source spelling and assigns it the same grammar and semantic meaning. A parser mode, post-parse exception, or phase-only AST variant may reject additional surface forms in Kio', but may never admit a Kio' form the full pipeline rejects.
- For surface-only productions (productions that exist in the surface grammar but not Kio'), confirm `prime::lower` rejects them with a parse error.

Findings:

- **Spec production with no parser path** — the EBNF says a shape is in Kio' but the parser doesn't accept it.
- **Parser path with no spec production** — the parser accepts a shape the EBNF doesn't list. Either the spec is incomplete or the parser is too liberal.
- **Kio'-only production or meaning** — `kio-prime` accepts a spelling, identifier class, AST case, or privilege that the full Kio pipeline rejects or interprets differently. Remove the Prime extension; updating the Kio' spec cannot make an extension into a subset.
- **Surface-only form not rejected by `prime::lower`** — a leak that lets a non-Kio' shape into the Kio' pipeline.

## 3. Map to the kio-prime-check-rs verifier

Walk `ci/infra/kio-prime-check-rs/src/lexer.rs` and `ci/infra/kio-prime-check-rs/src/parser.rs`. For each spec production:

- Locate the verifier path that accepts it.
- Cross-check rejection: shapes the verifier accepts that kio-rs's `prime::lower` rejects (or vice versa) are split.
- Cross-check the subset: every verifier-accepted form must also be accepted with the same meaning by the full Kio pipeline.

Findings:

- **Spec production accepted by kio-rs but rejected by the verifier** (or vice versa) — the two implementations disagree on Kio'. Decide whether the spec or one of the implementations is wrong.
- **Verifier admits a Kio extension** — the verifier and `kio-prime` agree on a form the full pipeline rejects or interprets differently. This is still a hard failure: two implementations agreeing cannot override the strict-subset contract.
- **Spec production no implementation accepts** — a dead EBNF rule.

## 4. Corpus probe

Run `sh ci/checks/orchestrators/golden-tests.sh` if the working tree builds it, and look at the kio-prime-marked cases: every `IS_KIO_PRIME` case under `test-data/goldens/` should parse identically under full `kio`, `kio-prime`, and the verifier. Any mismatch is a hard finding. Add a focused direct probe for each identifier/import/declaration class even when the corpus harness cannot route one source through all three consumers.

For productions that surfaced as "no implementation accepts" in § 3, check the corpus for a covering case. If none exists, that production is unexercised in the entire test surface — flag as a coverage gap (the audit's output overlaps with `audit-test-strategy` here; both should land it).

## 5. Cross-file consistency

Four rules interact:

- **Specs are contracts** ([`ai/topics/specs.md`](../../topics/specs.md)) — `prime.md` is a contract; `grammar.md` carries the canonical productions; the two cross-link rather than duplicate.
- **AGENTS.md § Universal rules — Kio' is a strict subset of Kio** — no parser mode, reserved spelling, verifier production, or Prime AST variant may add a form or privilege the full pipeline lacks.
- **AGENTS.md § Universal rules — Surface forms must not survive into Kio'** — the post-typecheck AST contains only Kio' constructs. The parser's rejection set in `prime::lower` is what enforces this on the input side.
- **AGENTS.md § Universal rules — Open-world compilation is non-negotiable** — any grammar production that resolves a name has to do so in an open-world-safe way; production-level checks here interact with `audit-open-world`'s deeper resolution-rule checks.

Walk recent surface-language commits — `git log --since="6 months ago" -- specs/prime.md specs/grammar.md kio-rs/src/pass/parser/ kio-rs/src/prime/ ci/infra/kio-prime-check-rs/src/` — and look for commits that touched one of these without paired updates to the others.

## How to report

Group findings into:

1. **Strict-subset violations** — Kio' or its verifier accepts a form, identifier class, AST case, or meaning the full Kio pipeline does not. Most serious.
2. **Four-view disagreements** — the full Kio pipeline, Kio' spec, kio-rs Prime pipeline, and verifier accept, reject, or interpret the same shape differently.
3. **Surface-only forms not rejected** — leaks into the Kio' pipeline at parse-or-lower time.
4. **Dead EBNF rules** — productions no implementation exercises.
5. **Drift candidates** — recent commits touching one of the three artifacts without paired updates.
6. **prime.md ↔ grammar.md divergence** — the Kio' EBNF appears in both pages and disagrees.

For each finding, cite spec section (`prime.md § Grammar` line range) and file:line in the implementation(s).

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

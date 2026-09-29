# No future extensions

On-demand reference for the universal rule **"Aspirational content lives only in `ROADMAP.md`"** (AGENTS.md § Universal rules): the scope, the class catalogue, and the boundary cases. Navigate here when writing public-facing prose that is tempted to mention the future.

## The rule and its scope

Public-facing artifacts describe the present. A spec states the current contract; a doc describes current behavior; a diagnostic reports a current rejection. Direction — what the project intends to build — lives in `ROADMAP.md` (approval-gated) and nowhere else.

The rule governs **public artifacts**: everything under `specs/` and `docs/`, README-class pages (`README.md`, `INSTALL.md`, `TESTING.md`, `CONTRIBUTING.md`, subproject READMEs), agent instructions (`AGENTS.md`, `ai/`), and user-facing CLI output (diagnostics, help text). It does **not** legislate internal code comments — those are governed by the comment discipline ([`comments.md`](comments.md)); see § Code comments below.

Duplicating direction outside `ROADMAP.md` creates unowned promises: they are not revisited when the roadmap changes, they read as commitments to outside readers, and they age into folklore ("that was always planned") or lies ("not yet supported" long after it shipped — or for something no one still intends to build).

## Inadmissible in public artifacts

- **Deferred-feature notes** — "X is a future extension", "not yet supported", "planned", "eventually", "for now", "until X lands / is specified". State the boundary as present-tense contract instead: what is accepted, what is rejected (with its error category), what does not exist.
- **Unbuilt components presented as real** — a tool, pass, or pipeline stage that does not exist, listed as a live consumer or dependency of a contract ("consumed by the website's snippet preprocessor" when no such preprocessor exists). A contract's consumer list names what exists.
- **Dev-process narration** — "the accepted subset grows as slices land", "the corpus is still being extended toward …". Public artifacts describe the repo's state, not its velocity.

## Admissible — current rules about the future

These reference the future but *are* present-tense contract; keep them:

- **Reservations** — a spelling, key, or numeric range rejected today so it can take on meaning later without breaking existing programs ("the `//…` operator family is reserved for future syntax"; "unused exit codes are reserved for future insertions"). A reservation states a *current rejection* and holds design space; it may name the reserved space generically but must not design the feature that might occupy it.
- **Concrete current-caveat banners** — a backend spec and host guide may mirror a specific known breakage and the host action it requires. Backend maturity labels and ad hoc stability banners such as “experimental” and “may evolve” are not admissible.
- **Host API compatibility status** — every host-backend spec and guide carries the exact mirrored field `Host API stability: evolving|stable`. `evolving` is a present compatibility policy with an approval gate, not a promise that a later feature or break will occur; no other status wording belongs there.
- **Forward-compatibility contracts** — statements binding what consumers may rely on ("the shape is additive — later versions may add exports; hosts consume only what their bridged modules declare").
- **Policy conditionals** — governance that pre-commits a process, not a feature ("if a future tool genuinely requires a different route, revise this policy in the same change").
- **References to `ROADMAP.md`** — any public artifact may point a reader at the roadmap for direction ("see `ROADMAP.md`", "`ROADMAP.md` tracks direction"); what it must not do is carry or restate direction the roadmap doesn't own.

The dividing line: an admissible mention **constrains or protects the present** (a rejection, a binding contract, a process rule); an inadmissible one **promises or narrates work that has not happened**.

## Code comments

Internal comments live under [`comments.md`](comments.md), not this rule. In particular, **extensibility and guard rationale that mentions the future is fine there**: "the struct stays a bundle so a future annotation can land here without restructuring", "asserted so a future cache-layout change can't silently re-root", "so a future regression trips this test loudly" all explain the present shape of the code. What a comment must not do is *park owed work* — "left for a future change", "not yet supported" — which is a partial-implementation smell, not a plan; [`audit-partial-implementations`](../skills/audit-partial-implementations/SKILL.md) owns that phrase list.

## Stating a deliberate omission

The absence of a feature is stated as contract, not apology: "There is no hex/binary/octal syntax", "diamond/shared resolution is never automatic", "there is no solver in the signature path". Phrase the omission so a reader knows the boundary is intentional and what the supported route is — never so it reads as a queue position.

## Sweep patterns

[`audit-no-future-extensions`](../skills/audit-no-future-extensions/SKILL.md) owns the drift sweep: grep the public artifacts above (excluding `ROADMAP.md`, and excluding audit skills that quote these phrases as patterns) for `future extension`, `not yet supported`, `future work`, `planned`, `eventually`, `someday`, `for now`, `until then`, `will be added`, `future version`. Its backend publication sweep also rejects `experimental`, `v1, may evolve`, “before stable”, and every ad hoc maturity or stability label under `specs/backends/` and `docs/hosts/`, while accepting only the exact Host API stability field. Every hit is one of the admissible classes above, a homonym false positive (e.g. the argument-slot planner's "planned slot" in `specs/language.md`), or a finding.

---
name: audit-comment-quality
description: Sweep code comments for drift — stale references to renamed/deleted symbols, contradictions with surrounding code, fabricated cross-references, WHAT-comments that just restate well-named identifiers, and negative-space comments that narrate what the code or language doesn't do
allowed-tools: Read, Grep, Glob, Bash
---

# Comment-quality audit

Code comments rot. [`ai/topics/comments.md`](../../topics/comments.md) is explicit about discipline at write-time ("default to no comments", "comment the WHY, not the WHAT", no task/fix/caller context) — but the project has no audit covering whether *existing* comments are still accurate. This skill is that audit.

The other audit skills touch comments tangentially: `audit-partial-implementations` catches stale-marker phrases (TODO / "for now" / "residual"); `audit-test-strategy` catches kio-rs internals in golden comments; `audit-skills` catches broken file references inside SKILL.md frontmatter. None of them check whether a comment that says *X does Y* is still true.

Read [`ai/topics/comments.md`](../../topics/comments.md) before starting; the comment-discipline heuristics this skill enforces (no WHAT-only comments, no historical residue, no commit-context references) reflect the project's settled style.

## 1. Stale symbol references

Grep `kio-rs/src/`, `specs/`, `docs/`, `tests/`, `ci/`, `ai/`, top-level `*.md` for code comments that name specific symbols — function names, type names, module paths, file paths, constants. For each hit, verify the named symbol still exists with that name:

- `crate::<path>::<symbol>` references → `grep -rn '<symbol>'` to confirm.
- `fn <name>` / `struct <name>` / `enum <name>` references → same.
- File path references (e.g. `see kio-rs/src/foo.rs`) → confirm the file exists.
- Spec section references (e.g. `per specs/language.md § X`) → confirm that section heading still exists in the spec.
- Cross-reference URLs / anchors (e.g. `README.md#some-anchor`) → confirm the anchor target.

A comment naming a thing that's been renamed, moved, or deleted is a finding. A comment claiming a spec section says X when it doesn't is a finding (the "fabricated cross-reference" pattern).

## 2. Contradictions with surrounding code

Harder to mechanically catch, but where comments quote behavior, check whether the code still does that thing. Heuristics for what to inspect:

- Doc-comments on functions / types that describe parameter or return shapes. Walk recent renames / signature changes (`git log --since="3 months ago" --diff-filter=M -- kio-rs/src/`) and check whether each touched item's doc-comment still matches the new shape.
- Comments inside match-arms or branches that describe what the arm handles. If the arm's pattern changed but the comment didn't, it's drift.
- Comments specifying numeric thresholds, format strings, error codes, etc., where the literal in the code differs from the literal in the comment.

This step is judgment-heavy; not every comment needs a recheck. Prioritize comments that quote spec wording, name specific identifiers, or describe a behavioral guarantee.

## 3. Fabricated cross-references

A specific drift pattern worth its own sweep: a comment links to a spec section / README section / other anchor with prose claiming the target says X, when the target says Y or doesn't say anything on the topic. Walk every `[...](path#anchor)` link in `kio-rs/src/` doc-comments and in `specs/`, `docs/` prose. For each, read the linking text's claim and the anchored content side-by-side. Mismatches are findings.

Often-fabricated patterns:

- "documented in README § ..." when the README section is silent on the claim.
- "per specs/X.md § Y" with a section heading that's been renamed or deleted.
- "see crate::foo::bar" with `bar` removed or relocated.

## 4. WHAT-comments and noise

`ai/topics/comments.md`: "Comment the WHY, not the WHAT." A comment that just restates the function / variable name in prose is noise. Heuristics for a sweep:

- Doc-comments on small helpers that describe their behavior in the same words as the function name (`/// Compute the sum of two integers.` on `fn sum(a: i32, b: i32)`).
- Inline comments that restate the line they precede (`// Set x to 5.` immediately above `x = 5;`).
- Long block comments whose content is dominated by what-it-does prose rather than why-it-exists prose.

These don't break anything, but they violate the project's comment-discipline rule, and worse — they're the comments most likely to rot first (because they're the ones tracking the code's *what*, not its *why*).

## 5. Comments referencing context the reader doesn't have

`ai/topics/comments.md`: comments shouldn't reference the current task, fix, or callers. Sweep for:

- `// added for X flow` / `// used by Y` — relationship references that belong in commit messages or PR descriptions, not in the code.
- `// fixes issue #N` / `// fixes the bug where ...` — same.
- `// previously this was ...` / `// before the fix ...` — historical residue that rot fastest.

Each is a finding (drop the comment).

## 6. Negative-space and contextual-residue comments

A distinct noise pattern, and the one most likely to be written *fresh* rather than to rot in place: a comment that narrates the *absence* of something, or that only makes sense in light of a conversation the author just had. The reader doesn't share that conversation, so the comment informs no one — and it ages badly, because the moment the absent thing arrives, the negation silently becomes false and nobody updates it.

Flag comments that:

- **State a settled language or design fact as a negation** — `// Kio has no block comments`, `// no multi-line comments here`, `// strings aren't interned`. The absence is the default; nothing in the surrounding code suggests otherwise, so naming it tells the reader nothing they'd have assumed differently.
- **Announce that the code does *not* do something, with no reason a maintainer would need** — `// not memoized`, `// we don't validate here`. A bare negation just restates the code's silence.
- **Preserve the residue of a design discussion** — `// we considered X but decided against it`, `// could use Y instead`. That belongs in a design note, commit message, or PR description, not the source.

The discriminator against a *legitimate* why-of-absence: a good negative comment names a concrete, still-true reason tied to *this* code that stops a maintainer from making a wrong "improvement" — `// no early return: the cleanup below must run on every path`, `// intentionally unbuffered — measured, buffer churn dominated for these tiny inputs`. Keep those. Delete the ones that merely announce an absence or echo a fact every reader already knows.

These leak most often from a contextual session: the author was just weighing whether to add the absent feature, so its absence felt salient enough to note. It wasn't. Finding = delete the comment (or, if a real constraint is buried in it, rewrite it as the concrete why).

## 7. Scope and pacing

This audit is broad; pick a sub-tree per pass rather than trying to walk everything at once. Reasonable starting points:

- Recently-modified files (`git diff @{u}..HEAD --name-only`, or any window).
- Files identified by other audits as having drifted (cross-pollinates with audit-spec-drift, audit-docs-drift).
- High-traffic kio-rs modules (lib.rs, prime/, typecheck_*, parser/) where comment density is highest.

Don't try to sweep all comments in one pass. Surface a triaged list per area.

## How to report

Group findings into:

1. **Stale symbol references** — file/line, the named symbol, what it should now be.
2. **Code/comment contradictions** — file/line, what the comment claims, what the code actually does.
3. **Fabricated cross-references** — file/line, the linking text's claim, what the target actually says.
4. **WHAT-comments** — file/line, suggested action (delete or rewrite as WHY).
5. **Context-leaking comments** — file/line, the relationship reference that should move to the commit message / PR description.
6. **Negative-space / residue comments** — file/line, the absence or design-discussion residue being narrated, suggested action (delete, or rewrite as a concrete why if a real constraint is buried).

For sweeping audits (many findings), prioritize by impact: contract surface > public API > internal helpers > tests / docs. Don't auto-fix WHAT-comments without checking they aren't load-bearing for the codebase's pedagogical style.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

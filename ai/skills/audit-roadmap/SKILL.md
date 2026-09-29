---
name: audit-roadmap
description: Verify ROADMAP.md hygiene — nothing "in flight" that's already settled, no bookkeeping phrasing, no premature specifics, no scratchpad links
allowed-tools: Read, Grep, Glob, Bash
---

# ROADMAP audit

ai/topics/roadmap.md lays down four rules. This skill verifies each:

- Consistent with `specs/` — no thread listed as in-flight that's already settled there.
- No leaked unsettled specifics (syntax, keywords, algorithms, guarantees still under discussion).
- No bookkeeping ("currently deferred", "not an immediate priority", "open question is X").
- No links to `scratchpad/`.

Read `ROADMAP.md` and ai/topics/roadmap.md before starting.

## 1. Threads vs specs

Read `ROADMAP.md` end to end. For each entry:

- Identify what design thread it represents.
- Search `specs/` for any page that already covers that thread.
- If the thread has settled into a spec, the entry should have been removed; flag it for removal.

Use `git log --since="3 months ago" -- specs/` to find recently-added spec content; cross-reference with current roadmap entries.

## 2. Premature specifics

Grep `ROADMAP.md` for the kind of detail that should stay in `scratchpad/`:

- Specific keyword/syntax names not already in `specs/grammar.md`.
- Pseudo-code, algorithms, type rules.
- Guarantees ("this will be sound", "this will be decidable") not yet stated in `specs/`.

Each is a finding — the roadmap is intent, not design.

## 3. Bookkeeping phrasing

Grep `ROADMAP.md` for `ai/topics/roadmap.md`'s prohibited phrases and similar:

- "currently deferred", "not an immediate priority"
- "open question", "TBD", "tbd"
- "blocked on", "waiting on"
- "status:", "phase", "milestone"
- "next step", "next slice"

Each is a finding.

## 4. Scratchpad links

Grep `ROADMAP.md` for `scratchpad/` or any reference to a gitignored path. [`ai/topics/roadmap.md`](../../topics/roadmap.md) and the no-leak rule ([`ai/topics/no-leak.md`](../../topics/no-leak.md) § Leak classes) forbid this — scratchpad is gitignored and any reference is broken externally.

## 5. Order conveys priority

ai/topics/roadmap.md says the file's order conveys rough priority. Without overstepping, check that nothing looks blatantly mis-ordered (a trivial item at the top, an obvious priority at the bottom). This is a judgment call — flag candidates rather than asserting.

## How to report

Group findings into:

1. **Settled threads still listed** — entries that should be removed because their content has landed in `specs/`.
2. **Premature specifics** — unsettled detail that should migrate back to `scratchpad/`.
3. **Bookkeeping phrasing** — weasel-words to delete.
4. **Scratchpad references** — broken external links.
5. **Suspect orderings** — judgment calls about priority order.

For each finding, cite the roadmap line and quote the offending text.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

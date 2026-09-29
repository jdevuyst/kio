---
name: audit-agents-md
description: Verify AGENTS.md structure and content placement — size cap, universal-before-first-action rules only, sharp triggers/pointers, resolving links, and no orphan topics
allowed-tools: Read, Grep, Glob, Bash
---

# AGENTS.md structure and placement audit

AGENTS.md is the always-loaded entry point for every agent in this repo. It carries three things and nothing else: universal rules, a trigger table, and a pointer set (see AGENTS.md § What belongs in AGENTS.md vs. `ai/topics/<topic>.md`). Long-form material migrates to `ai/topics/<topic>.md` files navigated to on demand. This skill enforces the structural and content-placement invariants that keep the always-loaded surface small and the topic-file system honest.

Read AGENTS.md before starting.

## 1. Size cap

`wc -c AGENTS.md` must be `≤ 40000`. If above, that's the headline finding — agents pay the size on every prompt, so the cap is load-bearing.

```sh
size=$(wc -c < AGENTS.md)
[ "$size" -le 40000 ] || echo "AGENTS.md is $size characters (cap: 40000)"
```

Report the actual byte count alongside the pass/fail. When over cap, the fix is to migrate the longest universal-rules paragraph or a section that crept in.

## 2. Trigger / pointer files exist

Walk AGENTS.md's trigger table and pointer set; every linked `ai/topics/<file>.md` must exist.

- For each `[...](ai/topics/<file>.md)` link in AGENTS.md, confirm the file is present at the path.
- Surface a broken-link finding when it isn't.

```sh
grep -oE 'ai/topics/[a-z0-9-]+\.md' AGENTS.md | sort -u | while read -r f; do
  [ -f "$f" ] || echo "AGENTS.md references missing file: $f"
done
```

## 3. No orphan topic files

Every file under `ai/topics/` must be referenced from `AGENTS.md`. Orphans are dead weight — content that drifts because no agent navigates to it.

```sh
find ai/topics/ -name "*.md" | while read -r f; do
  grep -qF "$f" AGENTS.md || echo "ai/topics/ topic file not referenced from AGENTS.md: $f"
done
```

A topic file that's only cross-linked from another `ai/topics/<topic>.md` (and never from the always-loaded `AGENTS.md`) is still an orphan from the agent-routing perspective — an agent that doesn't already know the topic exists can't navigate to it.

## 4. Cross-references resolve

<!-- markdownlint-disable-next-line MD038 -->
Sweep tracked files for the literal string `AGENTS.md § ` (with a trailing space) and verify each cited section still exists in AGENTS.md (top-level heading or bullet under § Universal rules), or has been updated to point at the appropriate `ai/topics/<topic>.md`.

```sh
git grep -n 'AGENTS\.md § '
```

For each hit, classify:

- **Resolves** — the cited section heading or bullet text still appears in AGENTS.md. Pass.
- **Stale** — the cited section no longer exists in AGENTS.md and the reference should be rewritten to either point at the new top-level heading, or at the file under `ai/topics/` where the content migrated.

Exclude `scratchpad/` (gitignored, not tracked; `git grep` already skips untracked files, so this is automatic).

Also verify the opposite direction: AGENTS.md's own outbound `specs/*.md § Heading` citations (in the Universal rules bullets) must resolve too — the checks above only catch other files citing `AGENTS.md § X`, not AGENTS.md citing a spec section that has since moved or been renamed.

The citations appear in two spellings — a backtick-labelled link (`` [`specs/<file>.md` § Heading](…) ``) and a plain link followed by the section (`[…](specs/<file>.md) § Heading`) — and spec paths may carry subdirectories and uppercase (`specs/formal/prime.md`, `specs/backends/README.md`). Parse the raw line around those literal Markdown delimiters and keep the citation as input data rather than interpolating it into a command, regular expression, or `awk -v` assignment. Normalize Markdown code-span backticks only after capture. The backtick-labelled spelling is bounded by its exact `](` link delimiter and matches the complete cited heading, allowing only the target heading's documented ` — ` suffix. A plain-link citation has no closing delimiter, so it can run into following prose; its heading may end before sentence punctuation followed by whitespace or end of line.

AGENTS.md's outbound `ai/topics/<file>.md § Heading` citations (a rule bullet pointing at the topic section that owns its rationale) must resolve the same way. Parse the raw line around the literal `(ai/topics/` opener and `) §` link/section delimiter, admit only a direct lowercase topic filename, and keep the citation as input data rather than interpolating it into a command, regular expression, or `awk -v` assignment. Normalize Markdown code-span backticks only after capture. A citation resolves at the exact heading boundary, at the documented ` — ` heading-tail boundary, or before sentence punctuation followed by whitespace or end of line.

For both citation classes, code-span normalization removes only matching
delimiter runs; content backticks remain literal. Heading-tail and sentence
delimiters count only outside code spans. Scan source references only on
top-level bullets under `## Universal rules` and table rows under
`## Trigger table`; each eligible line has independent Markdown code-span
state. Ignore eligible-looking source lines inside fenced examples. Only target
ATX headings with one to six `#` markers and outside fenced code blocks count
as section headings. Run the shared validator:

```sh
awk -f ci/checks/repo-lint/audit-agents-md-headings.awk AGENTS.md
```

## 5. Content belongs in the always-loaded layer

Read every bullet under `## Universal rules`, plus every trigger and pointer
added or materially expanded in the caller-supplied range. Without an explicit
range, use dirty changes plus the default unpushed range (`@{u}..HEAD`, or
`main..HEAD` when the branch has no upstream). If neither gives a bounded
change, skip this origin-sensitive subcheck rather than widening it into a
historical sweep. Classify each change against AGENTS.md's own placement
contract:

- A universal rule belongs only when an agent needs its complete norm before
  the first action in essentially every conversation.
- An action-specific rule belongs in its owning topic behind a sharp trigger.
- A catalogue belongs in the pointer set.
- A task-, session-, machine-, incident-, or maintainer-specific operating
  tactic does not become tracked guidance merely because it can be rewritten
  without paths, numbers, or names.

The fresh-clone portability test is necessary but not sufficient. Generic
prose can still encode one recovery incident or preferred workflow as a
permanent `always`/`never` rule. Inspect the surrounding change and available
authority record for new mandates. Accept either (a) a repo-verifiable
instruction correction under AGENTS.md's tooling-friction rule, or (b) a
portable, durable experience-derived convention the user approved as tracked
guidance. A direction about the current run is not, by itself, check-in
authority. If the needed interaction record is unavailable, report authority
as unverified and blocking rather than inventing either approval or a leak.
Conversely, do not flag a repository invariant merely because it is
conditional or operational.

Report the exact rule, which of AGENTS.md's three admitted roles it fails to
occupy, and the correct disposition: delete it, keep it only in untracked
session state, or move durable action-specific detail behind an existing sharp
topic trigger.

## How to report

Group findings into:

1. **Size cap breach** — `AGENTS.md` over 40,000 characters. Report actual size and the section most likely worth migrating.
2. **Broken trigger / pointer links** — AGENTS.md links to a non-existent `ai/topics/<file>.md`.
3. **Orphan topic files** — `ai/topics/<file>.md` present but not referenced from AGENTS.md.
4. **Stale cross-references** — `AGENTS.md § <X>` citations whose target no longer exists in AGENTS.md.
5. **Content-placement violations** — situational tactics or topic-level detail in the universal core, or non-sharp routing entries.

For each finding, cite file and line where applicable.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

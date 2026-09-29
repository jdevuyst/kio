---
name: audit-no-leak
description: Sweep governed public artifacts, excluding hand-written blog post bodies, for leaked session/machine/user context — scratchpad or worktree paths, machine specs, identities and private rationale, orchestration narration, agent-guidance links, session-relative dates — plus the agent-layer portability classes
allowed-tools: Grep, Glob, Bash
---

# No-leak audit

AGENTS.md § Universal rules ("Checked-in files are written for outside readers — keep them context-free") and [`ai/topics/no-leak.md`](../../topics/no-leak.md): the governed **public** project artifacts are published to outside readers (clones, GitHub) and must carry no session/machine/user-specific context. Hand-written `docs/blog/*.md` post bodies are explicitly exempt. This skill sweeps the governed artifacts for each leak class and classifies every hit.

These are mostly small, mechanical greps. Worth their own skill because the rules are hard and the checks trivially scale over the repo.

## Scope

The rule governs the **public artifacts** only. Restrict every grep to:

- `specs/`, `docs/` except `docs/blog/*.md`, `test-data/`, `kio-rs/`, `ci/`
- `README.md`, `ROADMAP.md`, `CONTRIBUTING.md`, `TESTING.md`
- commit messages (sweep the unpushed window, `git log @{u}..HEAD`, when reviewing a branch)

**Classify the agent-guidance layer separately.** It may describe workflow mechanics, but it is not a blanket exemption for incidental local paths, host-specific values, or session state. Prefer portable wording in `AGENTS.md`, `ai/`, and `.gitignore`; concrete local configuration belongs in untracked local files.

For the public-artifact sweep, exclude the agent-guidance layer:

```sh
PUB=':!AGENTS.md :!ai/ :!.gitignore :!docs/blog/*.md'
```

`git grep` limits to tracked content by default, so each sweep below already ignores `scratchpad/` (gitignored).

Then run § 8 over the agent-guidance layer: workflow mechanics are its subject matter, so only the narrower portability classes apply there (identity, machine-state, and preference-mandate classes).

## 1. Scratchpad paths

The original case. Search the public artifacts for the gitignored notes directory:

```sh
git grep -n 'scratchpad' -- $PUB
```

Every hit is a finding (the directory is gitignored — any reference is broken for external readers and leaks internal naming). Also watch for the softer forms: "see notes" / "see design" / "deferred design" / "draft" / "WIP design" without an in-repo anchor — if the notes are actually in `scratchpad/`, the reference is implicitly broken. Classify each: direct path reference (remove or migrate), prose word-use (judgment — generic word vs. naming the directory), code comment (remove; comments must never reference scratchpad content).

## 2. Worktree / filesystem paths

Developer-checkout, local file URL, and worktree paths a reader doesn't
have:

```sh
git grep -nE 'file://|path[[:space:]]+"(/|~|[A-Za-z]:[\\/])|/tmp/|kio-worktrees|/home/[[:alnum:]_.-]+|/Users/[[:alnum:]_.-]+|(^|[[:space:]])~/' -- $PUB
```

Findings: absolute paths into a checkout, absolute `.dep.kio` `source`
paths, local `file://` URLs, `/tmp/…` scratch paths, home-directory
paths, sibling-worktree paths. Judgment: a doc that genuinely documents
temp-file *behavior* may legitimately mention `/tmp`, and a spec that
documents accepted git clone URL schemes may legitimately mention
`file://` — classify by whether the path names *this developer's
environment* (leak) or *the artifact's own subject* (fine).

## 3. Machine / box specs

Hardware properties of the developer's box:

```sh
git grep -niE 'small box|[0-9]+-core|[0-9]+ ?cores|[0-9]+ ?(gb|gib) ?(ram|mem)|oom' -- $PUB
```

Findings: core counts, RAM, "on a small box", OOM-avoidance narration. Judgment: a *property of the code or algorithm* ("O(n) in module count") is fine; naming *this machine's* resources is the leak.

## 4. User identity and private rationale

The user's identity in prose, or the private *why-the-user-wanted-it*. Derive the identity tokens from the repo's own history at audit time — never hardcode a maintainer's name or address in this file (a hardcoded identity would itself be the leak class this section polices, and it silently skips every other maintainer):

```sh
names=$(git log --format='%an' | sort -u | grep -viE 'bot|github-actions' | tr ' ' '\n' | awk 'length($0) >= 4' | sort -u)
emails=$(git log --format='%ae' | sort -u | grep -viE 'noreply|github-actions')
idents=$(printf '%s\n' "$names" $(printf '%s\n' "$emails" | sed -E 's/@.*/@/') $(printf '%s\n' "$emails" | sed -E 's/^[^@]*//') | grep -v '^$' | sort -u | paste -sd'|' -)
git grep -niE "(${idents})|to save (money|cost)|the user (wants|wanted|prefers|dislikes|asked)|for cost reasons|business" -- $PUB
```

(The derived tokens are the maintainers' name words, email local-parts (`local@`), and email domains (`@domain`); an unescaped `.` in a domain overmatching is harmless — every hit gets classified anyway.) Findings: a maintainer's name/email in prose (a `Signed-off-by` / `Co-Authored-By` trailer in a commit is the sanctioned exception — not a finding); cost/business/preference motive. Describe what changed and the technical reason, never the user's private motive.

## 5. Agent-orchestration mechanics

How the work was coordinated, which belongs in the agent-guidance layer, not a public artifact:

```sh
git grep -niE 'background agent|integration agent|fan(-| )out|ran [0-9]+ agents|in parallel across|sub-?agent|worktree' -- $PUB
```

Findings: "we ran N agents", agent/session IDs, fan-out narration, "the integration agent merged". Judgment: "parallel" in a genuine concurrency spec is fine; agent/worktree *coordination* narration is the leak.

## 6. Agent-guidance links from public docs

Public reader-facing files must not point readers into the agent-guidance layer:

```sh
git grep -nE 'AGENTS\.md|ai/topics/|ai/skills/' -- \
  README.md ROADMAP.md INSTALL.md CONTRIBUTING.md TESTING.md \
  ':(glob)specs/**/*.md' ':(glob)docs/**/*.md' \
  ':(exclude,glob)docs/blog/*.md' \
  ':(glob)test-data/**/*.md' ':(glob)ci/infra/**/*.md'
```

Findings: links from public docs, specs, README / CONTRIBUTING / TESTING, test-data READMEs, or public crate READMEs to agent instructions. If the target material is genuinely public, move or summarize it in a public artifact and link there; otherwise remove the link. Judgment: generated fixtures or tests may include these strings only when the string itself is the subject under test; contribution-policy pages may name agent-only paths when the path itself is the policy subject.

## 7. Session-relative dates and framing

Anchors that meant something only during the authoring session:

```sh
git grep -niE '\b(today|yesterday|this (session|week|sprint)|as of (this|now)|right now|currently|recently|just (landed|added|fixed))\b' -- $PUB
```

Findings: "today", "this week", "currently", "as of this session", "recently". A published file is read at an unknown future time. State the durable fact (a version, an unconditional present-tense description), not the relative one. Judgment: "currently" describing a *true present-tense* invariant of the system can be fine; the leak is the *session-relative* anchor that ages out. The `ROADMAP` in-flight / bookkeeping form is also covered by `audit-roadmap`.

## 8. Agent-guidance layer — portability classes

The agent-guidance layer (`AGENTS.md`, `ai/`, `.gitignore`) may describe workflow mechanics, so the full public-artifact bar does not apply; sweep it for the narrower portability classes in [`ai/topics/no-leak.md`](../../topics/no-leak.md) § Leak classes ("Machine-state diagnoses phrased as guidance", "Maintainer-preference mandates") and § The portability filter:

```sh
AGENT='AGENTS.md ai/ .gitignore'
git grep -niE "(${idents})" -- $AGENT
git grep -nE '/home/[[:alnum:]_.-]+|/Users/[[:alnum:]_.-]+|(^|[[:space:]])~/[[:alnum:]]' -- $AGENT
git grep -niE '[0-9]+-core|[0-9]+ ?cores|[0-9]+ ?(gb|gib) ?(ram|mem)' -- $AGENT
```

(`$idents` is § 4's derived token list — identities, absolute local paths, machine specs.) Classify each hit with the portability filter's fresh-clone litmus: does the sentence hold on a fresh clone on any machine (a repo fact or symptom-conditional guidance — fine), or does it encode this machine's state or one maintainer's workflow as law (finding)? Maintainer-preference mandates rarely grep mechanically — while classifying, read surrounding "always / never" workflow prose for one-workflow-stated-as-law phrasing. A detection pattern whose literal string is the subject (e.g. this file's own greps) is not a finding.

Portable wording is necessary but not sufficient. For each agent-layer mandate
added or materially expanded in the caller-supplied range—or, by default, in
dirty changes plus `@{u}..HEAD` (falling back to `main..HEAD`)—check whether it
originated as a task-, session-, machine-, or incident-specific instruction.
Do not widen this origin check into a historical sweep when no bounded change
exists. Removing concrete details does not turn current-run direction into a
repository convention. Accept repo-verifiable instruction corrections and
portable, durable experience-derived conventions explicitly approved as
tracked guidance. When the interaction record needed to distinguish those
routes is unavailable, report authority as unverified and blocking rather than
classifying the text as a proven leak.

## How to report

Report each hit as: file, line, the offending substring, leak class (§ 1–8), classification (leak vs. legitimate-subject judgment call), suggested fix (remove / reword / migrate / make durable). When sweeping a branch's commit messages, note those separately.

If every sweep is clean across the public artifacts, report "clean."

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

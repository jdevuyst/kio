---
name: audit-blog-history
description: Audit blog posts as a chronological communication record — build a claim and retraction ledger, verify every unretracted Kio claim against current or historically pinned repository evidence, and report contradictions or unverifiable claims
allowed-tools: Read, Grep, Glob, Bash
---

# Blog history audit

Anchor: [`ai/topics/docs.md`](../../topics/docs.md) § Blog posts — factual
communication remains active unless it is time- or version-scoped or a later
post explicitly retracts, corrects, or supersedes it.

This is a truth-maintenance audit, not an editorial review. Read every source
post completely and in publication order, preserve the chronology, and report
only. Blog post bodies remain approval-gated; never edit one as part of this
audit without the user's specific authorization for that post change.

## 1. Establish the corpus

Run `sh ci/checks/repo-lint/blog-lint.sh`, but continue the truth audit if the
mechanical check fails. Enumerate the existing `docs/blog/YYYY-MM-DD-*.md`
source files from the working tree and sort them bytewise by filename, oldest
first. Include untracked posts and exclude deleted posts and generated output
under `docs/out/`.

Read every post in that order. Do not sample, start from the newest post, or
review posts independently: later prose can alter the status of an earlier
claim only after the earlier claim has entered the ledger.

## 2. Build the claim ledger

Record every checkable factual claim about Kio or this repository. For each
claim retain:

- source post and line;
- a short normalized proposition;
- scope: unqualified/current, timeless, release/version-scoped, or explicitly
  dated;
- evidence needed;
- status: active, retracted, corrected, or superseded.

Claims include feature availability, guarantees, supported backends and tools,
compiler architecture, test and audit behavior, release facts, and exact
counts. Do not turn personal opinion, rhetoric, hopes, clearly stated plans, or
predictions into factual claims. A plan becomes auditable only if the prose
also claims that its promised result already exists.

Interpret an explicitly dated or version-scoped statement in that historical
scope. It remains true or false as a claim about that point in history; normal
later evolution does not make it stale. Treat an unqualified or timeless claim
as active against the current repository.

## 3. Apply later communication

Process each later post against the ledger. Retire an earlier claim only when
the later post clearly identifies the earlier communication or proposition and
explicitly retracts, corrects, or supersedes it.

- A newer conflicting statement without that acknowledgement is an
  **unretracted contradiction**, not an implicit replacement.
- A partial correction retires only the corrected portion.
- A retraction is itself an active factual claim and must agree with the
  repository evidence.
- Ambiguous wording does not erase an earlier claim; report the ambiguity only
  when it affects whether two factual claims can coexist.

## 4. Verify active claims

Use primary repository evidence: `specs/`, the implementation, manifests,
backend contracts, CI scripts, and runtime corpus coverage. Prefer the narrowest
authoritative source for each proposition.

For a release- or date-scoped claim, inspect the full commit pinned by the
post's relevant repository links with `git show` / `git cat-file`, or use local
history around that release when the post has no relevant link. Do not judge a
historical claim solely from today's tree. If the necessary commit or evidence
is unavailable locally, report a verification gap rather than guessing or
silently fetching history.

Require high confidence for a contradiction. Distinguish:

- **contradicted active claim** — current authoritative evidence makes an
  unretracted claim false;
- **unretracted inter-post contradiction** — two posts cannot both be true in
  their stated scopes and the later post does not explicitly correct the first;
- **invalid retraction/correction** — the later communication disagrees with
  authoritative evidence;
- **verification gap** — a checkable claim lacks enough local evidence to
  establish truth.

Do not report an opinion merely because the audit disagrees with it, or a plan
merely because it changed.

## 5. Report

Report findings in chronological order within these categories:

1. Contradicted active claims.
2. Unretracted inter-post contradictions.
3. Invalid retractions or corrections.
4. Verification gaps.

For each finding cite the post and line, quote the minimum necessary claim,
state its ledger scope and status, cite the contradicting or missing evidence,
and name any later post that was considered as a possible retraction.

Summarize valid retraction chains separately so the reader can see which old
communication was intentionally retired. If every active claim verifies and
every contradiction has an explicit valid retraction, report `clean`.

**Default: report only.** A fix directive does not override the blog-post
approval gate.

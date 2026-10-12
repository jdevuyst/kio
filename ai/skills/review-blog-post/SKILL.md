---
name: review-blog-post
description: Review hand-written blog posts for objective editorial errors, Kio facts, commit-pinned links and Kiodoc validation; suggest compilation fixes and harnesses without editing the post or replacing its author's voice
allowed-tools: Read, Grep, Glob, Bash
---

# Blog post review

Blog posts under `docs/blog/` are written by hand by the maintainers — unlike the
AI-generated rest of the docs — and are approval-gated. This skill gives a
**light-touch editorial review** and **reports findings**; it never edits a post.
The author decides what to apply.

The guiding principle is restraint: **preserve the author's voice**, surface the
objective mistakes worth fixing, and err on the side of *not* pushing the author
to change things. When a call is borderline, stay silent — "reads clean" is a
good and common result, and is better than manufacturing notes.

Blog post bodies are exempt from [`ai/topics/no-leak.md`](../../topics/no-leak.md).
They may carry personal or session-relative context and may link to any tracked
path in the repository, including agent guidance. Repository file and directory
links must be GitHub URLs pinned to a full commit object ID
(`blob/<40-hex-commit>/…` or `tree/<40-hex-commit>/…`), never a branch or tag such
as `main`.

Unlike `specs/` and the rest of `docs/`, **a blog post may be forward-looking or
aspirational** — do not flag "coming soon" / "we plan to" prose as an error the
way a specs audit would. A stated intention is not a false claim.

## 1. Select the posts to review

Default: every blog post **not yet on `origin/main`** — the set about to be
published. Take the union of:

- committed-but-unpushed changes — `git diff --name-only origin/main...HEAD -- docs/blog/`
- uncommitted working-tree changes — `git status --porcelain -- docs/blog/`

Keep only `docs/blog/*.md` post files (ignore deletions). If `origin/main` is not
available locally, say so and fall back to `--last`. If the resulting set is
empty, report "no unpushed blog posts" and offer `--last`.

Arguments override the default:

- a slug or path — review just that post;
- `--all` — every `docs/blog/*.md`;
- `--last` — only the most recent post (largest `YYYY-MM-DD-` filename).

For an existing post, compare the proposed final contents with its prior
published contents and inspect intermediate revisions where needed. Apply
[`docs.md` § Blog posts](../../topics/docs.md#blog-posts): distinguish
meaning-preserving language/format corrections and small code/API updates from
substantive revisions. Formatting changes are substantive when formatting is
the post's subject. Judge the claims and example's meaning, not changed-line
counts. Substantive revisions need a visible notice with the edit date and
brief nature; report a missing or misleading notice and propose exact text
without editing. Disclose unavailable publication/history evidence rather than
assuming the current text proves what readers previously saw. Exact-text
approval, authorial voice and the report-only boundary still apply.

## 2. Mechanical and Kiodoc checks

Run the CI checks that cover blog posts first, so the editorial pass never
re-litigates mechanical issues the pipeline already gates:

- `sh ci/checks/repo-lint/blog-lint.sh` — the blog authoring contract (filename
  shape and valid date, a single H1, a standard byline linking to a GitHub
  profile, commit-pinned links to repository files and directories, straight
  quotation marks and apostrophes in the Markdown source).
- `markdownlint-cli2 <post>` for each reviewed post — Markdown mechanics, the
  same tool and `.markdownlint.json` config that
  `ci/checks/repo-lint/markdown-lint.sh` runs repo-wide.

Also validate every selected post with a compiler matching the reviewed
checkout, following [`local-tools.md`](../../topics/local-tools.md) for local
compiler invocation. Set `KIO_BIN` to its absolute path; from the repository
root, substitute each selected post's filename in these commands:

```sh
(cd docs && "$KIO_BIN" doc check "blog/POST.md")
(cd docs && "$KIO_BIN" doc fmt --check "blog/POST.md")
```

Use `--check`, never the rewriting form of `doc fmt`. These are the same
Kiodoc checks that CI applies recursively to the docs package, narrowed to the
selected posts. Old posts must meet their snippet expectations with current
Kio too; historical prose does not authorize using an old compiler or excluding
the post. `doc check` validates snippets and their declared error expectations;
it does not execute snippets or verify displayed runtime output. Report those
limits and any ignored or otherwise unvalidated Kio examples.

If a mechanical check fails, report it and skip the prose-editorial pass for
that post. Still report Kiodoc results where the document can be parsed, and
investigate compilation failures as below. No failed check authorizes an edit.

### Verify every link before sign-off

Enumerate final-post links, including reference definitions, autolinks, images,
HTML links and fragments. Verify local destinations against their actual
publication routes and check anchors in the rendered target. Verify external
HTTP destinations with bounded requests and inspect redirects; retry a rejected
HEAD as GET or use a browser when necessary. A status code alone does not prove
that a redirect, login wall or soft 404 is the intended destination. Validate
non-HTTP links by their applicable syntax and destination evidence; do not send
messages or perform other side effects as a link check.

For ordinary repository file/directory links, verify the exact full commit,
path and fragment and establish that a release tag resolves to that commit and
has a published, non-draft release in the canonical repository. A local tag or
an ancestor of a release is insufficient. Commit/diff/issue/pull-request links
are separate subjects and need no release tag. Keep full commit URLs; version
labels supplement their identity. This is a sign-off check, not a requirement
to rewrite unchanged old posts or add live network checks to every broad gate.

Record checked destinations, methods, redirects and release evidence with the
review. Rate limits, authentication barriers and unavailable destinations are
unverified, not passes; resolve them before declaring sign-off readiness.
Never silently delete or replace a link.

When a content link uses `main` or another moving ref, offer to resolve and
insert the full commit ID for the latest published release with a release tag.
Show the release version, tag, resolved commit and exact replacement URL for
approval; do the Git lookup rather than making the author find the hash. Verify
the path and fragment at that revision. Compare the linked file (or directory)
with current `main` and warn about any changes since the release, including
renames or deletion. If the target is absent from that release or its contents
no longer support the post, explain the mismatch instead of silently choosing
an older release or an unreleased commit. Applying the approved replacement is
a separate edit, followed by renewed affected review; this review remains
report-only.

## 3. What to flag

Review each selected post's prose. Group findings by category, most objective
first.

### Always surface — mechanics (objective)

- **Spelling** errors.
- **Grammar** errors.
- **Punctuation** errors.
- **Quotation-mark convention** — blog source uses straight ASCII `"` and `'`,
  not curly quotation marks or apostrophes. Report the exact characters and
  suggest straight replacements; do not apply them. This does not prohibit
  other Unicode text or punctuation. If faithful quoted material or code needs
  those characters, present the conflict for the author instead of silently
  changing it.

Quote the exact text and give the correction. This is the skill's core value.

### Always surface — facts and repository links (objective)

- **Factual errors about Kio** — a claim that contradicts the specs or the
  implementation: a feature described that doesn't exist, wrong behavior, a
  mis-stated guarantee. Cross-check against `specs/` and the code, and cite the
  contradicting source. **High confidence only** — flag an outright false
  statement; stay silent on anything arguable or a matter of emphasis. Mind the
  aspirational carve-out above.
- **Repository links** — a post may link anywhere in the tracked repository,
  including `AGENTS.md` and `ai/`. For each GitHub link to a file or directory in
  this repository, require `blob/<40-hex-commit>/…` or
  `tree/<40-hex-commit>/…`. Flag a branch, tag, short hash, or other moving ref;
  do not flag issue, pull-request, commit, release, or profile URLs as content
  links.

### Note sparingly — clarity (optional)

- A genuine **clarity or grammatical improvement**, but only where something is
  actually confusing or wrong — never a preference. Mark each clearly as optional
  ("take it or leave it") and keep them few. **Never** propose a reword that
  changes the author's tone or voice, and never invent a house style beyond
  the explicit blog authoring rules.

### Kio examples — compilation findings and suggested harnesses

For each failing example, explain the diagnostic and distinguish missing
surrounding context from incorrect visible code or a language-contract conflict.
Suggest a concrete fix for the author, including ready-to-apply hidden Kiodoc
harness declarations and fence attributes when those supply genuine omitted
imports, host declarations, wrappers or support files. Use the existing
[`Kiodoc contract`](../../../specs/kiodoc.md), not a second testing mechanism.
Validate a proposed harness in a disposable package with the required context
when possible, and distinguish tested suggestions from unverified ones.

Visible code remains hand-written: propose corrections just as for prose, never
rewrite it. Hidden harness text is also part of the protected post body. Do not
replace the example's behavior with a stub, substitute different visible logic,
or turn a supposed-to-work example into an ignored or expected-error case to
make validation pass. Report formatting fixes rather than applying them.

If a post and current language behavior conflict, present the failure and
alternatives for a maintainer decision. A CI failure authorizes neither a post
rewrite nor a language compatibility shim. Preserve the author's prose and
code until a specific edit is approved; this review skill itself remains
report-only.

## 4. Report

Produce a per-post findings list. Make no edits to the `.md`. For each post:

- the objective mechanics (spelling / grammar / punctuation), each with its fix;
- the factual findings, each with the contradicting source, and unstable
  repository-link findings;
- for existing posts, the compared revisions, meaning-based edit classification
  and notice adequacy, or unavailable history;
- link verification results, exact release correspondence for repository
  content links, proposed moving-ref replacements and newer-content warnings,
  and any unresolved destination or fragment;
- the Kiodoc check/format results, compilation fixes or harness suggestions,
  and which proposed fixes and visible examples were actually validated;
- then, cordoned off and clearly labelled optional, the sparse clarity notes.

If a post has no objective findings, say so plainly rather than reaching for
something to comment on.

## Sign-off readiness

A commit introducing or changing a post may receive DCO sign-off only after this
review succeeds for its final text, code and harnesses. Report readiness
explicitly: mechanical, Kiodoc and link checks pass, objective findings are resolved,
and validation limits are disclosed. Optional editorial suggestions may be
declined. Record the reviewed post's blob ID or content hash and compiler
revision with the review result; later edits invalidate affected review/checks.
An existing trailer is not proof of review. Successful review supplies neither
DCO certification authority nor permission to edit, push or deploy.
For publication, use [`publish-website`](../publish-website/SKILL.md).

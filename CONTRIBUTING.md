# Contributing to Kio

Kio is a maintainer-driven project, built almost entirely with AI under maintainer direction and review: the language design and the implementation are the maintainers', and we don't accept code pull requests against them — a deliberate choice about review capacity and design coherence. The sections below describe the contributions we do take.

## Bug reports

The most useful contribution is a precise bug report — ideally a minimal `.kio` file that should compile or run but doesn't (or compiles and then misbehaves). There are two ways to send one:

- **As an issue** — open the **Bug report** template, paste the source inline along with the command you ran, the target (`js`, `rust`, …), what you expected, and what happened. Include the output of `kio --version` (or the commit hash you built from). A maintainer turns the reproducer into a regression test.
- **As a pull request** — add the reproducer directly to the test corpus as a `KNOWN_FAILING` contributed case under your name (see below). It's marked expected-to-fail, so it doesn't break CI; when a maintainer fixes the bug they remove the marker and it becomes a passing test. This is the fastest path from report to fix, and your case stays in the tree.

## Contribute an example program or library — the one pull request we accept

The one pull request we accept adds a case under [`test-data/contrib/`](test-data/contrib/). Once merged it joins Kio's CI, so every future change to the language has to keep it working — durable coverage against regressions. Each case lives in a directory named after its author, and it's one of:

- **an example program** — runnable Kio that builds, runs, and prints what you expect;
- **a library** — Kio with an exported API and no `main`, proven by `equiv` laws;
- **a bug reproducer** — a case that *should* work but doesn't, marked `KNOWN_FAILING` (the bug-report-as-PR path above; file it as a **Bug report** rather than here).

The flow:

1. **Open a "Contribute an example program or library" issue** (or a **Bug report** issue, for a reproducer) describing what your case demonstrates. The issue number `N` names your directory.
2. **Create `test-data/contrib/<your-github-username>-<N>/`** following the case contract in [`test-data/contrib/README.md`](test-data/contrib/README.md).
3. **Open a pull request** that touches only that directory, with `Closes #N` in the body.

A PR that touches only `test-data/contrib/` and links its issue stays open; anything else is closed automatically (see [What happens to other PRs](#what-happens-to-other-prs)). A status check runs on the PR immediately: it verifies the directory name against your username and linked issue plus the file-level case contract, labels the PR `contrib case` once it's in shape, and comments with anything that needs fixing. Review beyond that checks the case contract, not design taste — this lane is deliberately low-friction. A few practical notes:

- One case per PR, and keep your branch up to date with `main` by rebasing, not merging. Only you can open follow-up PRs against your own case directory.
- Commits need DCO sign-off like everything else — see [below](#developer-certificate-of-origin-dco).
- **A program or library must succeed; a bug reproducer must be marked `KNOWN_FAILING`.** An unmarked case that fails, or a marked case that passes, is a contract error the check flags.
- **Prefer `run.args`** (a program) or **`run.test-only`** (a library). A `run.sh` drives its own tooling and runs arbitrary shell, so it's executed only after a maintainer reviews it — expect it to be picked up more slowly.
- **Building and running your case on the PR is maintainer-initiated.** A maintainer applies the `test-contrib` label to run it; any push you make clears that approval, so they re-apply it after re-reviewing.
- **After merging, maintainers keep the case working.** If a language change affects it, a maintainer adapts it as part of that change.

## Ideas and feature requests

Ideas are welcome as conversation starters — use the **Feature idea** template. Know what it is going in: Kio's design is maintainer-driven, so an idea issue opens a discussion, not a pull request. If a maintainer wants to take the idea up, they'll follow up on the issue.

## Why we don't take code PRs

The compiler, the specs, and the tooling are dense, tightly coupled artifacts where careful review of an outside change routinely costs more than writing the change — and several areas (published editor extensions, instructions loaded silently by tooling) widen the blast radius of a subtle bug well beyond this repo. Keeping the code maintainer-written keeps the design coherent.

Every path in the repo except `test-data/contrib/` is maintainer-only. If you've found a bug, report it (an issue, or a reproducer PR into the contrib lane); if you have a design proposal, file an issue and a maintainer will pick up the implementation.

In rare cases a maintainer may explicitly invite a PR on a specific issue by labeling it `accepting prs`; that label is the only other thing that keeps a PR open. There's no need to ask for it — if a maintainer wants to hand work out, they'll say so on the issue.

## What happens to other PRs

A non-draft PR that is neither a contrib-lane PR (it touches only `test-data/contrib/`, and either links its issue or updates a case directory that already exists — a follow-up) nor linked to an issue labeled `accepting prs` is closed automatically — immediately when it's opened or updated, and otherwise by a weekly scan once it's at least 48 hours old. Sanctioned PRs get their sanctioning label mirrored onto the PR by the status check (`contrib case` or `accepting prs`), so standing is visible at a glance. Bot-authored PRs are exempt. If a close crosses paths with a just-applied label or a just-linked issue, reopen the PR and it'll be re-checked.

## Developer Certificate of Origin (DCO)

Every commit must be signed off under the [Developer Certificate of Origin] version 1.1. Sign-off is your statement that you have the right to submit the change under this project's license; it is not a copyright assignment.

Add a sign-off automatically with `git commit -s`. The trailer looks like:

```
Signed-off-by: Jane Doe <jane@example.com>
```

The name and email must match a real identity you would be willing to vouch for the contribution under. Anonymous or pseudonymous sign-offs are not accepted.

A GitHub DCO check rejects pull requests containing any commit without a valid sign-off.

[Developer Certificate of Origin]: https://developercertificate.org/

### DCO 1.1 (full text)

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

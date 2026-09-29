# Contributed cases

Driven by [`ci/checks/orchestrators/contrib-tests.sh`](../../ci/checks/orchestrators/contrib-tests.sh).

This corpus is the contribution lane described in
[`CONTRIBUTING.md`](../../CONTRIBUTING.md) § Contribute an example program or library —
the one place in the repo where external pull requests are accepted.
Each case is small, self-contained, contributed by a community member,
named after them, and run as part of CI from then on: every future
change to Kio has to keep it working.

A case is one of three shapes:

- **a program** — runnable Kio with a `main` that builds, runs, and
  prints what you expect;
- **a library** — Kio with an exported API and no `main`, proven by
  `equiv` laws (like a POC, but contributed);
- **a bug reproducer** — a program or library that *should* work but
  doesn't, marking a bug you've found. It's expected to fail until a
  maintainer fixes the bug, so it doesn't redden CI in the meantime.

## Directory naming

`test-data/contrib/<github-username>-<issue-number>/`, all lowercase —
for example `octocat-123/` for the case @octocat proposed in issue
`#123`. The issue reserves the slot; the PR links it with `Closes #N`.
GitHub usernames are case-insensitive, so lowercasing yours loses
nothing. Only the named contributor may open follow-up PRs against
their own case directory — the policy check enforces the match.

## Case contract

A case directory contains, at the top level (flat — no nested case
directories):

- `README.md` — a short description of what the case demonstrates.
- `workdir/` — the Kio package: a `<pkg>.pkg.kio` with a
  `build { … }` block, plus the module(s) providing `main` (a program)
  or the exported API (a library). Self-contained: no dependencies
  (`*.dep.kio`) — everything the case needs lives in this directory.
- **exactly one execution file** — the shape the case takes:
  - `run.args` — the standard build-then-run path (normally empty). For
    a **program**.
  - `run.test-only` — `kio test` + `kio build`, no runner. For a
    **library**: it must declare at least one `equiv` law, which
    `kio test` discharges. (An empty marker file.)
  - `run.sh` — a custom script that drives its own tool invocations. No
    other `*.sh` file is allowed, and a `run.sh` case is run on a PR
    only after a maintainer reviews it (see below); prefer `run.args`
    or `run.test-only` when they cover your case.
- `expected.stdout` — the case's exact stdout (empty for a library).
- exactly one stderr-policy file — usually an empty
  `expected.stderr.ignore`; see
  [`test-data/README.md`](../README.md) for the three policies.
- `expected.exit` — containing exactly `0`, the *desired* outcome. A
  normal case reaches it; a bug reproducer records the exit it *should*
  reach once fixed (also `0`).
- `input.stdin` — only if the program reads stdin
  (`read_ascii_line()`), and required then.
- `IS_KIO_PRIME` — only if the package is Kio' (the low-level core
  form); required then, forbidden otherwise. Most natural surface Kio
  is not Kio', so most cases omit it.
- `KNOWN_FAILING` — only for a **bug reproducer**: an empty marker that
  tells CI the case is *expected* to fail. The gate stays green and the
  run prints a warning; if the case ever passes (the bug got fixed but
  the marker wasn't removed) CI fails, so stale markers can't linger.
  Because the marker is per-case (not per-target), a `KNOWN_FAILING`
  case must reproduce on *every* target it declares — so declare **only**
  the target(s) where the bug actually occurs (a front-end bug is all of
  them; a codegen bug may be one). On a target where it unexpectedly
  passes, CI reads that as a stale marker and fails.

Those files plus `workdir/` are the *only* entries allowed in a case
directory: no stray files, no extra directories, no other `*.sh`, and
no symlinks. Under `workdir/`, every file is a `.kio` source. CI
rejects anything else, so a contrib PR can carry no content beyond Kio
source, the fixed fixtures above, and (for a `run.sh` case) the one
reviewed script.

Declare at least one `target` in the `build { … }` block; for a normal
case, declaring more makes it cover more of Kio's backends. (A
`KNOWN_FAILING` reproducer is the exception — it declares only the
target(s) where the bug occurs; see above.) The shipping target ids
are `js`, `ts`, `python`, `java`, `rust`, `go`, `swift`, and `haskell`.

Run `kio fmt` over your sources before submitting — CI asserts every
tracked `.kio` file is at the formatter's fixed point. (A
`KNOWN_FAILING` case is exempt from the per-case checks, since a case
that doesn't build can't satisfy them.)

## How your case runs on a PR

When you open the PR, the policy check validates the structure above
(naming, ownership, files, allowlist) and flags a `run.sh` or
`KNOWN_FAILING` case for the maintainer. Once merged, the case joins
the corpus that runs on every CI pass.

Building and running your case *on the PR itself* is maintainer-gated,
because it executes your code. A maintainer applies the `test-contrib`
label to run your case at its current commit; any push you make clears
that approval, so they re-apply it after reviewing your change. A
`run.sh` case runs arbitrary shell, so expect it to be picked up more
slowly than a `run.args` or `run.test-only` case.

## Quality bar

Deliberately relaxed relative to the other corpora. A contrib case is
natural Kio a user would actually write — it does not need the
minimized shape of a golden, the adopter-grade API surface of a POC,
or the scale of a castle, and it is not subject to those corpora's
contracts. What it must not contain: absolute paths, machine- or
user-specific content, or (outside a `KNOWN_FAILING` reproducer)
workarounds for compiler bugs.

## Maintenance

After merge, maintainers keep contributed cases green: when a language
change affects a case, a maintainer adapts it as part of that change.
When a maintainer fixes the bug a `KNOWN_FAILING` case reproduces, they
remove the marker in the same change, turning it into a normal passing
case — and pin the fix with a focused regression golden under
[`test-data/goldens/`](../goldens/): bugs get fixed, not hidden. The
contributed case keeps its natural source shape and its attribution.

---
name: audit-runner-arg-safety
description: Verify no run.args token can feed the per-backend test runner an abusable command-line option, and that contrib run.sh scripts stay within the maintainer-reviewed sandbox — the untrusted-contribution attack surface.
allowed-tools: Read, Grep, Glob, Bash
---

# Runner argument-safety audit

`test-data/contrib/` accepts external pull requests, so a contrib case's
`run.args` and `run.sh` are **untrusted input** until a maintainer reviews and
runs them. This skill verifies that input can't turn the per-backend test runner
into a weapon — the concern the contrib lane's threat model turns on. The other
corpora (`goldens/`, `poc/`, `castles/`) are maintainer-authored, so the same
sweep there is drift-detection rather than defence, but run it too: a dangerous
    pattern shouldn't exist anywhere. Emissions are also maintainer-authored,
    but their contract forbids `run.args` entirely: each case owns one
    success-only `run.sh` and the orchestrator supplies a rejecting runner
    tripwire.

## 1. The runner argument surface

Establish what the runner is *supposed* to receive, from the code, not memory:

- `ci/run-tests.sh` `execute_case` invokes `"$runner" <prepared run.args
  tokens…> out/<target>` (then the build-output dir). Most run.args tokens are
  passed literally; the harness consumes
  `--artifact-namespace <target-id>=<namespace>` and passes only the selected
  target's namespace to the target-local runner CLI.
- `prepare_run_args` (`ci/run-tests.sh`) already constrains tokens to the
  character class `[A-Za-z0-9._/@=+:-]` — no shell metacharacters, so shell
  injection is closed. What is **not** constrained is *which flags / paths*
  reach the runner.
- Read the runner's own argument handling — the parsing lives in the per-backend
  bins (`ci/infra/kio-test-runner-rs/src/bin/kio-test-runner-*.rs`), with shared
  helpers under `src/shared/`. Establish the full set of flags it interprets; as
  of writing that is `--protocol <name>`, `--profile <name>` (the optimization
  profile, validated then ignored where a
  backend has no levels), `--package-name <name>` (normally injected by the
  harness rather than written in `run.args`), and target-local
  `--artifact-namespace <effective-namespace>` attached to the preceding
  package descriptor, plus `-h`/`--help`;
  the output dir is positional (exactly one, or exactly two for `--protocol
  coexist`). Re-derive this from the code, since flags can be added.

The safe shape of a `run.args` file is therefore: **empty**, or the runner's
known flags with expected values — `--protocol <registered-name>` and, where
used, `--profile <name>` / the harness-level portable
`--artifact-namespace <target-id>=<namespace>`. Every selected namespace value
must pass the runner backend's public namespace grammar before it is used as an
artifact path, import, or compiler argument; accepting `/`, `..`, an absolute
path, or a backend-invalid namespace or identifier is a high-severity finding. A token that
is neither a known flag nor its expected value is a finding to explain or fix.

## 2. Sweep run.args across the corpora

Before classifying tokens, report any `test-data/emissions/**/run.args` as a
corpus-layout finding. It must be removed, not allowlisted: emission scripts do
not invoke the runner, and a runner argument would blur the fixed-host/artifact
assertion boundary.

```sh
find test-data/emissions -name run.args -type f -print
```

For the remaining language corpora:

```
git grep -l '' -- 'test-data/**/run.args' | while read -r f; do
  # awk (not `tr < "$f"`) so a file with no trailing newline can't merge
  # its last token with the next file's first token.
  awk '{ for (i = 1; i <= NF; i++) print $i }' "$f"
done | sort -u
```

For every distinct token, classify it:

- `--protocol` or a protocol name registered in
  `ci/infra/kio-test-runner-rs/README.md` / `src/shared/protocol.rs` — safe.
- `--artifact-namespace` plus a target-id-qualified namespace selected by
  `ci/run-tests.sh`, then accepted by `src/shared/artifact_identity.rs` under
  the current runner backend's public grammar — safe.
  Verify the parser rejects traversal and does not merely rely on the shell
  token character allowlist.
- A path token (`/`-bearing, or containing `..`), an unknown `--flag`, or
  anything that isn't the protocol selector — **finding**: a run.args entry
  that reaches the runner as an unexpected argument. Check what the runner does
  with it; a path-traversal (`..`) or an absolute path is the highest-severity
  case.

Cross-check that every protocol name named in a `run.args` actually exists in
the runner's protocol registry (a typo'd or removed protocol is a separate
finding, and a name the runner doesn't recognise is exactly the "unexpected
argument" case).

## 3. Contrib run.sh scrutiny

A `run.sh` runs arbitrary shell, accepted only because a maintainer reviews it
before applying the `test-contrib` label (see `ai/topics/contributing.md` and
`.github/workflows/contrib-run.yml`). Verify the guard rails around it hold:

- `pr-policy-check.yml` flags a run.sh case with an advisory comment, and
  `contrib-run.yml` runs only on the maintainer-applied `test-contrib` label
  with the label stripped on every push. Confirm both still do so — a run.sh
  case that could run *without* the maintainer gate is the worst finding here.
- Sweep `test-data/contrib/**/run.sh` for shell that escapes the case sandbox:
  network egress (`curl`, `wget`, `nc`, `/dev/tcp`), writes outside the case
  dir, `eval` of fetched content, or reads of host paths outside the checkout.
  These aren't auto-forbidden (a maintainer may accept one), but each is a
  finding to justify.
- Confirm the file allowlist (`validate_contrib_contract` and
  `pr-policy-check.yml`) still rejects every `*.sh` other than `run.sh`, so a
  case can't smuggle a second script the reviewer might skim past.

## How to report

Group findings into:

1. **Abusable run.args** — a token that reaches the runner as an unexpected
   flag or path (path-traversal / absolute path first).
2. **Missing gate** — a contrib run.sh path that could run without the
   maintainer label, or an allowlist hole letting a non-`run.sh` script in.
3. **Sandbox-escape run.sh** — network / out-of-case-write / eval patterns to
   justify or remove.
4. **Emission contract drift** — any emission `run.args`, runner invocation, or
   attempt to turn its independently owned host/artifact assertion into a
   runner protocol.

For each, cite the file and the exact token / line.

**Default: report only.** If invoked with a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

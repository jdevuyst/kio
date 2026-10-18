---
name: upgrade-deps
description: Reason-driven dependency bumps (Cargo, npm, mise toolchains, GitHub Actions) — security advisory, EOL/unmaintained line, or deprecation going fatal; absorbs the mechanical fallout keeping ci/all.sh green; stops and reports when a fix would be more than mechanical
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Skill
---

# Upgrade dependencies (kio)

Bump the repo's *pinned* dependencies and absorb the fallout, keeping `ci/all.sh` green. A doer, not an `audit-*` — those ask "is the checkout self-consistent?" against in-repo ground truth; this reconciles the checkout against the *outside world* (what's been superseded, gone EOL, or deprecated upstream), so its verdict changes when reality moves, not when the checkout does.

Run builds and tests in the provisioned development environment through
`ci/impl-toolchain.sh toolchain-exec <languages> --core -- COMMAND` so project
language tools and ordinary mise helpers share explicit selection; see
[local-tools.md](../../topics/local-tools.md). Stable report/audit commands use
`toolchain-exec rust --environment optional --reports -- COMMAND` after
`install-report-tools`.
Do not let missing selected tools fall back to account compilers. Mise upstream
discovery and lock regeneration need the tracked configuration and online
maintenance context, rather than verified execution's offline configuration
snapshot; keep these operations outside `toolchain-exec`.

For native mise discovery/lock commands, start an ordinary fresh shell without
`toolchain-env` exports or an enclosing `toolchain-exec`, then run from the
checkout root:

```sh
repo_mise=$(sh ci/impl-toolchain.sh toolchain-bootstrap)
"$repo_mise" trust mise.toml
MISE_AUTO_INSTALL=0 MISE_EXEC_AUTO_INSTALL=0 "$repo_mise" outdated --bump --local
MISE_AUTO_INSTALL=0 MISE_EXEC_AUTO_INSTALL=0 "$repo_mise" -E optional outdated --bump --local
```

Repeat discovery for the other tracked environments. `--bump` compares beyond
exact configured requests; `--local` excludes unrelated account configuration.
Discovery does not authorize a bump or raising a compatibility floor.

Use that same pinned executable for `lock` or `-E ENVIRONMENT lock` after
editing tracked configuration. These operations intentionally use the original
checkout configuration and network, not a verified immutable snapshot. They
do not require an account mise installation. Reinstall affected environments
through the shared owner before returning to verified build/test execution.

## The invariant

Every upgrade leaves the tree green under `ci/all.sh`, or it doesn't land. That is the whole point of the skill — not "bump and let CI catch it later," but "bump, keep it green, and prove it." Iterate the fix loop with the narrowest gating coverage (see [`local-ci.md`](../../topics/local-ci.md) and [`TESTING.md`](../../../TESTING.md) § Local iteration); run **full `ci/all.sh` as the final gate** before the work is done.

For toolchain bumps the invariant has teeth: emitted code must behave per spec
on the actual selected runtime. Run backend goldens under the affected tools
and retain focused native host-interface witnesses where the ordinary runner
embeds another runtime. In particular, selecting Node does not change the
QuickJS runtime embedded by the ordinary JS/TS runner. Compilation or a checker
pass is not runtime verification.

## What counts as a dependency — discover, don't hard-code

Don't trust a baked-in list; *discover* the surface each run, so a newly-added crate or package can't slip through. The only fixed thing is a small set of **ecosystem handlers** — each a (how-to-find, how-to-bump) pair — applied to whatever the repo actually contains:

| Ecosystem | Find | Bump |
| --- | --- | --- |
| Cargo | every tracked `Cargo.toml` | edit the manifest, then run `sh <repo-root>/ci/cargo.sh update -p <crate>` from that workspace |
| npm | every tracked `package.json` (outside `node_modules/`) | edit the manifest, regenerate `package-lock.json` |
| mise | tracked `mise*.toml [tools]` files | edit the mise config, then `mise lock` or the matching `mise -E <env> lock` |
| Delegated toolchains and bootstrap | tracked manifests and bootstrap pins under `tools/toolchains/` | update the owning native manager/child request and verified upstream artifact or metadata identities; use the shared provisioning handler |
| GitHub Actions | `uses:` pins in `.github/workflows/*.yml` | repin the SHA / tag |

Discover with e.g. `git ls-files '*Cargo.toml' '*package.json' 'mise*.toml' 'tools/toolchains/*'`, each mise `[tools]` table, delegated handlers' manifest reads, and the workflows' `uses:` lines — never a hard-coded path list. Include exact native environment overlays and their locks. The handler set above is all that's fixed, and [`audit-upgrade-deps`](../audit-upgrade-deps/SKILL.md) pins it against reality: every real manifest *kind* has a handler, and every handler still resolves to a real manifest.

Mise's manager pin does not cover delegated child compilers: discover those
requests and their metadata/checksum owners separately, including GHCup/GHC and
Rustup bootstrap. Reinstall/verify affected environments through the shared
owner, retaining side-by-side selection and configuration/default nonmutation.
The [compatibility refresh](../refresh-target-language-compatibility/SKILL.md)
maintains minimum/current-tested endpoint evidence; that distinct task does not
relax this skill's reason-driven upgrade policy or authorize raising a generation
baseline's public floor.

Historical compatibility-test endpoints are deliberate test subjects, not
development-tool recommendations. Do not apply security or EOL upgrades to
them automatically: replacing an old endpoint loses evidence for that version.
Still assess exposure and follow the compatibility refresh skill's bounded-use
rules; if safe execution is unavailable, report the evidence gap rather than
silently updating or skipping the endpoint. Ordinary development and release
tools remain subject to the upgrade policy below.

Out of scope: Kio's *own* repo-wide version (the mirror list in `ci/checks/repo-lint/version-check.sh`) — not a dependency, and `audit-versioning`'s domain. Leave it alone.

## Upgrade needs a reason

"A newer version exists" is **not** a reason; chasing it is the churn this skill exists to avoid. Upgrade only on:

- **Security** *(patch axis)* — an advisory against the pinned version (`cargo audit` / RUSTSEC, `npm audit` / GHSA, GitHub advisories for actions and toolchains; GitHub's Dependabot *alerts* are the passive backstop between runs). Take it *on sight* — at whatever granularity the fix ships (usually a patch / minor) and across a major if that's where it lives. A security advisory is always a reason, so the no-routine-patch rule below never suppresses it.
- **End-of-life / unmaintained** *(major-line axis)* — the pinned *line* no longer gets fixes. `endoflife.date` tracks major lines (node 20, go 1.22, ghc 9.10), so it catches a dead line, never patch lag within a live one — that's Security's axis. Determine it as a cascade: `endoflife.date` (covers node / go / rust / ghc) → if the toolchain isn't there (only Swift today), its release-policy page → if there's *genuinely* no signal and no LTS, track a current release rather than an unverifiable old pin ("no data" is usually *us not checking*, so verify before falling back). For libraries, crates.io / npm yank + deprecation flags.
- **Deprecation going fatal** — a feature you depend on is being removed. Check upstream runtime and retirement notices as well as actionlint / zizmor and build warnings. Inspect pinned actions' runtime metadata, including nested actions in composites; passing workflow linters does not establish runtime support.

When a reason *does* force a move, prefer the **LTS** line where the toolchain has one — node's even-numbered series, GHC's LTS — for its long support window and fewer future forced bumps; toolchains without LTS (go, rust, swift) take the latest *stable* release.

Explicitly **don't**: routinely bump a patch/minor of an otherwise-fine maintained dep, or chase a new *major* while your current line is still maintained — *unless* a reason (almost always Security) calls for it.

Discover candidates with the mise commands above, `npm outdated`, and `sh <repo-root>/ci/cargo.sh update --dry-run` from each Cargo workspace, plus the advisory / EOL sources above — then filter to the ones with a reason. Security and unmaintained advisories come from `npm audit` (built into npm) and Cargo's `cargo audit`, invoked from the relevant workspace as `sh <repo-root>/ci/cargo.sh audit`. `cargo audit` isn't part of cargo and isn't a standing core toolchain tool; it is pinned as `cargo:cargo-audit` in `mise.optional.toml` and installed on demand with the optional Cargo-backed tools ([`local-tools.md`](../../topics/local-tools.md) § Toolchain provisioning policy): `sh ci/impl-toolchain.sh install-report-tools`. It runs *only* here, never as a CI gate — a security advisory is external, time-varying state, so gating `ci/all.sh` on it would redden unrelated PRs the day an advisory drops.

**"No reason" is a checked conclusion, not a default.** It means *no reason detected after running every detector you have* (the audits, `endoflife.date` / the release-policy page, the deprecation linters) — never a proof of safety. "`npm audit` clean" is "no *published* advisory," not "secure"; `endoflife.date` silence is "maintained" *or* "not tracked." A detector that's missing or inconclusive (no EOL data, an unqueryable advisory source, a transitive dep's unknown maintenance) is an **unknown** — it routes to the blind-spot fallback or gets flagged, it does *not* count as clear. So the per-candidate check is mandatory, and the report names which detectors ran — a "stayed put" must mean "checked and clear," not "didn't look."

## Bump and adapt

Apply the bump, regenerate the lock (`sh <repo-root>/ci/cargo.sh update -p <crate>` from the affected Cargo workspace, the npm lock, `mise lock`), and fix what breaks — **mechanically**. A fix adapts the consumer to the dependency's changed API or behavior (a rename, a changed signature, a moved import, a clippy adaptation). It never reshapes Kio to dodge a failure:

- If a bump surfaces a *real* bug — a clippy promotion exposes a genuine issue, a runtime release makes emitted code violate spec — **fix the bug and land a focused regression golden**, per `AGENTS.md` § Universal rules ("Bugs surface; never hide them"; a bug fix lands with a dedicated golden). Don't silence the lint or loosen the golden.
- Don't reshape source or goldens to pass — `AGENTS.md` § Universal rules, "Goldens demonstrate behavior, not workarounds."

## When to stop

If keeping the tree green needs more than a mechanical fix — a real migration across many call sites, a semantic redesign, or a bug you can't cleanly fix-and-golden — **stop and report**. Don't half-land it; a partial slice trips `AGENTS.md` § Universal rules, "No partial implementations." Hand back a writeup: what you set out to bump, the reason, where it got stuck, and what a full fix would take.

## Deciding deferrals — the skill decides, it doesn't ask

### No compatible upstream fix is available

When an advisory affects a dependency but no upstream-supported compatible fix
has been released, retain the pin, bring the finding to the user's attention,
and continue the already-authorized work, including release publication. Do not
wait for an unavailable update or require a separate exception approval. Examples
include Firefox fixes not yet available in the browser build supported by
Playwright, and announced Node.js security patches that have not been released.

Verify upstream availability rather than assuming it. Report the affected
version, known severity and remaining uncertainty, why the fix is unavailable,
and where the dependency is used or distributed, including devcontainers.
Record that disposition with the dependency/release evidence; it is not a clean
security result or a claim that the vulnerability cannot be exploited. Recheck
availability when finalizing release readiness and at the next dependency review.

This rule does not cover an available fix that is merely inconvenient or needs
a migration, nor a failed or inconclusive detector. Use the normal upgrade and
stop rules for those cases. Do not force an unsupported browser/runtime pairing
to manufacture a nominal upgrade.

### Available fixes and disproportionate deferrals

An available fix for a real advisory with a plausible exploit path in shipped
or runtime code is *taken*, subject to the migration stop rule above. But many
findings have **no practical impact and only a disproportionate fix** —
re-surfacing those for the user every run is busywork. Decide these yourself;
don't ask.

**Defer** — silently, on your own judgment — only when **all three** hold:

1. The advisory is informational / unmaintained, *or* its exploit vector is structurally absent in how the dep is used (e.g. serializing the project's own data, not attacker input).
2. The code is never compiled into a shipped artifact — target-conditional, or a dev/test-only dependency.
3. The only available fix is disproportionate — a major bump of an *unrelated* dependency, or a breaking change needing full re-validation, for no real security gain.

Otherwise: a cheap fix → **take it**; a genuine judgment call → **escalate to the user**. The unavailable-fix rule above is report-and-proceed, not a silent deferral. Never silently defer a real vulnerability with a plausible vector in runtime code.

Re-apply the disproportionate-deferral criteria each run; do not grandfather an
earlier decision. Only findings meeting all three criteria above stay out of
the report. Unavailable-fix findings remain reported and recorded as specified
above; new advisories or changed circumstances are assessed normally.

## Report

Summarize for the user: which deps moved and the *reason* for each, what code fixes the bumps demanded (and any regression goldens added), what stayed put and why, and anything that hit the stop condition and needs them.

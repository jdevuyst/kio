---
name: upgrade-deps
description: Reason-driven dependency bumps (Cargo, npm, mise toolchains, GitHub Actions) — security advisory, EOL/unmaintained line, or deprecation going fatal; absorbs the mechanical fallout keeping ci/all.sh green; stops and reports when a fix would be more than mechanical
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Skill
---

# Upgrade dependencies (kio)

Bump the repo's *pinned* dependencies and absorb the fallout, keeping `ci/all.sh` green. A doer, not an `audit-*` — those ask "is the checkout self-consistent?" against in-repo ground truth; this reconciles the checkout against the *outside world* (what's been superseded, gone EOL, or deprecated upstream), so its verdict changes when reality moves, not when the checkout does.

Run it in the mise-provisioned toolchain — the devcontainer, or any host with `mise` active. The `mise outdated` / `mise lock` steps need `mise` on PATH; the Cargo / npm / Actions handlers use their native tools and work without it.

## The invariant

Every upgrade leaves the tree green under `ci/all.sh`, or it doesn't land. That is the whole point of the skill — not "bump and let CI catch it later," but "bump, keep it green, and prove it." Iterate the fix loop with the narrowest gating coverage (see [`local-ci.md`](../../topics/local-ci.md) and [`TESTING.md`](../../../TESTING.md) § Local iteration); run **full `ci/all.sh` as the final gate** before the work is done.

For toolchain bumps the invariant has teeth: Kio's correctness is emitted code behaving *per spec on the host runtime*, so a `node` / `go` / `swift` / `ghc` / `rust` / wasm point release is verified by the **backend goldens actually running** under `ci/all.sh`. "It compiles" is not "it's verified."

## What counts as a dependency — discover, don't hard-code

Don't trust a baked-in list; *discover* the surface each run, so a newly-added crate or package can't slip through. The only fixed thing is a small set of **ecosystem handlers** — each a (how-to-find, how-to-bump) pair — applied to whatever the repo actually contains:

| Ecosystem | Find | Bump |
| --- | --- | --- |
| Cargo | every tracked `Cargo.toml` | edit the manifest, then run `sh <repo-root>/ci/cargo.sh update -p <crate>` from that workspace |
| npm | every tracked `package.json` (outside `node_modules/`) | edit the manifest, regenerate `package-lock.json` |
| mise | tracked `mise*.toml [tools]` files | edit the mise config, then `mise lock` or the matching `mise -E <env> lock` |
| GitHub Actions | `uses:` pins in `.github/workflows/*.yml` | repin the SHA / tag |

Discover with e.g. `git ls-files '*Cargo.toml' '*package.json' 'mise*.toml'`, each mise `[tools]` table, and the workflows' `uses:` lines — never a hard-coded path list. The handler set above is all that's fixed, and [`audit-upgrade-deps`](../audit-upgrade-deps/SKILL.md) pins it against reality: every real manifest *kind* has a handler, and every handler still resolves to a real manifest.

Out of scope: Kio's *own* repo-wide version (the mirror list in `ci/checks/repo-lint/version-check.sh`) — not a dependency, and `audit-versioning`'s domain. Leave it alone.

## Upgrade needs a reason

"A newer version exists" is **not** a reason; chasing it is the churn this skill exists to avoid. Upgrade only on:

- **Security** *(patch axis)* — an advisory against the pinned version (`cargo audit` / RUSTSEC, `npm audit` / GHSA, GitHub advisories for actions and toolchains; GitHub's Dependabot *alerts* are the passive backstop between runs). Take it *on sight* — at whatever granularity the fix ships (usually a patch / minor) and across a major if that's where it lives. A security advisory is always a reason, so the no-routine-patch rule below never suppresses it.
- **End-of-life / unmaintained** *(major-line axis)* — the pinned *line* no longer gets fixes. `endoflife.date` tracks major lines (node 20, go 1.22, ghc 9.10), so it catches a dead line, never patch lag within a live one — that's Security's axis. Determine it as a cascade: `endoflife.date` (covers node / go / rust / ghc) → if the toolchain isn't there (only Swift today), its release-policy page → if there's *genuinely* no signal and no LTS, track a current release rather than an unverifiable old pin ("no data" is usually *us not checking*, so verify before falling back). For libraries, crates.io / npm yank + deprecation flags.
- **Deprecation going fatal** — a feature you depend on is being removed. actionlint / zizmor already flag this for workflows; build warnings flag it for toolchains.

When a reason *does* force a move, prefer the **LTS** line where the toolchain has one — node's even-numbered series, GHC's LTS — for its long support window and fewer future forced bumps; toolchains without LTS (go, rust, swift) take the latest *stable* release.

Explicitly **don't**: routinely bump a patch/minor of an otherwise-fine maintained dep, or chase a new *major* while your current line is still maintained — *unless* a reason (almost always Security) calls for it.

Discover candidates with `mise outdated`, `mise -E optional outdated`, `npm outdated`, and `sh <repo-root>/ci/cargo.sh update --dry-run` from each Cargo workspace, plus the advisory / EOL sources above — then filter to the ones with a reason. Security and unmaintained advisories come from `npm audit` (built into npm) and Cargo's `cargo audit`, invoked from the relevant workspace as `sh <repo-root>/ci/cargo.sh audit`. `cargo audit` isn't part of cargo and isn't a standing core toolchain tool; it is pinned as `cargo:cargo-audit` in `mise.optional.toml` and installed on demand with the optional Cargo-backed tools ([`local-tools.md`](../../topics/local-tools.md) § Toolchain provisioning policy): `sh ci/impl-toolchain.sh install-report-tools`. It runs *only* here, never as a CI gate — a security advisory is external, time-varying state, so gating `ci/all.sh` on it would redden unrelated PRs the day an advisory drops.

**"No reason" is a checked conclusion, not a default.** It means *no reason detected after running every detector you have* (the audits, `endoflife.date` / the release-policy page, the deprecation linters) — never a proof of safety. "`npm audit` clean" is "no *published* advisory," not "secure"; `endoflife.date` silence is "maintained" *or* "not tracked." A detector that's missing or inconclusive (no EOL data, an unqueryable advisory source, a transitive dep's unknown maintenance) is an **unknown** — it routes to the blind-spot fallback or gets flagged, it does *not* count as clear. So the per-candidate check is mandatory, and the report names which detectors ran — a "stayed put" must mean "checked and clear," not "didn't look."

## Bump and adapt

Apply the bump, regenerate the lock (`sh <repo-root>/ci/cargo.sh update -p <crate>` from the affected Cargo workspace, the npm lock, `mise lock`), and fix what breaks — **mechanically**. A fix adapts the consumer to the dependency's changed API or behavior (a rename, a changed signature, a moved import, a clippy adaptation). It never reshapes Kio to dodge a failure:

- If a bump surfaces a *real* bug — a clippy promotion exposes a genuine issue, a runtime release makes emitted code violate spec — **fix the bug and land a focused regression golden**, per `AGENTS.md` § Universal rules ("Bugs surface; never hide them"; a bug fix lands with a dedicated golden). Don't silence the lint or loosen the golden.
- Don't reshape source or goldens to pass — `AGENTS.md` § Universal rules, "Goldens demonstrate behavior, not workarounds."

## When to stop

If keeping the tree green needs more than a mechanical fix — a real migration across many call sites, a semantic redesign, or a bug you can't cleanly fix-and-golden — **stop and report**. Don't half-land it; a partial slice trips `AGENTS.md` § Universal rules, "No partial implementations." Hand back a writeup: what you set out to bump, the reason, where it got stuck, and what a full fix would take.

## Deciding deferrals — the skill decides, it doesn't ask

A real advisory with a plausible exploit path in shipped or runtime code is *taken*, never deferred. But many findings have **no practical impact and only a disproportionate fix** — re-surfacing those for the user every run is busywork. Decide these yourself; don't ask.

**Defer** — silently, on your own judgment — only when **all three** hold:

1. The advisory is informational / unmaintained, *or* its exploit vector is structurally absent in how the dep is used (e.g. serializing the project's own data, not attacker input).
2. The code is never compiled into a shipped artifact — target-conditional, or a dev/test-only dependency.
3. The only available fix is disproportionate — a major bump of an *unrelated* dependency, or a breaking change needing full re-validation, for no real security gain.

Otherwise: a cheap fix → **take it**; a genuine judgment call → **escalate to the user**. Never silently defer a real vulnerability with a plausible vector in runtime code.

Re-apply this each run — nothing is persisted. A deferred advisory simply isn't surfaced (the criteria re-decide it); anything that *fails* the test — a new advisory or a changed situation — surfaces normally.

## Report

Summarize for the user: which deps moved and the *reason* for each, what code fixes the bumps demanded (and any regression goldens added), what stayed put and why, and anything that hit the stop condition and needs them.

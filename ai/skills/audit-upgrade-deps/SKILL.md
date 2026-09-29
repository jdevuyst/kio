---
name: audit-upgrade-deps
description: Verify upgrade-deps handler coverage — every dependency-manifest kind in the repo has a handler (no-orphan), every handler still resolves to a real manifest (no-dangling), and its named sources and tools exist
allowed-tools: Read, Grep, Glob, Bash
---

# upgrade-deps coverage audit

[`upgrade-deps`](../upgrade-deps/SKILL.md) discovers the repo's dependency surface from a fixed set of **ecosystem handlers** (Cargo → `Cargo.toml`, npm → `package.json`, mise → tracked `mise*.toml [tools]`, Actions → workflow `uses:` pins) and bumps what it finds. Discovery keeps it current for new manifest *instances* automatically; what it cannot self-detect is a new manifest *kind* it has no handler for, or a handler that has gone stale. This audit pins the handler set against reality. The shape mirrors [`audit-agents-md`](../audit-agents-md/SKILL.md) §2–§3 (no-dangling + no-orphan).

## 1. No orphan — every real manifest kind is handled

Enumerate every dependency manifest in the repo and confirm its ecosystem is one `upgrade-deps` handles. An unhandled kind is a manifest whose dependencies no bump path covers — the "someone added Python and nothing watches it" finding.

```sh
git ls-files \
  | grep -iE '(^|/)(Cargo\.toml|package\.json|mise[^/]*\.toml|pyproject\.toml|go\.mod|Gemfile|requirements\.txt|mix\.exs|pom\.xml|build\.gradle[^/]*)$' \
  | grep -vE '(^|/)node_modules/'
```

Cargo, npm, and mise are handled today (GitHub Actions too, but its pins aren't a manifest file, so it won't appear above). Any other kind — a `go.mod`, a `pyproject.toml` — is an **orphan finding**: `upgrade-deps` needs a new handler, or the manifest needs a stated exemption.

## 2. No dangling — every handler resolves

The reverse: each ecosystem `upgrade-deps` claims to handle must still have at least one real manifest.

- Cargo → `git ls-files '*Cargo.toml'` non-empty.
- npm → at least one tracked `package.json` outside `node_modules/`.
- mise → at least one tracked `mise*.toml` present with a `[tools]` table.
- GitHub Actions → at least one `.github/workflows/*.yml` carrying a `uses:` pin.

A handler with no surviving manifest is a **dangling finding** — the ecosystem left the repo and the handler should be retired from the skill.

## 3. References resolve

`upgrade-deps` names canonical sources and tools. Confirm each still resolves:

- The files it points at exist: `ci/checks/repo-lint/version-check.sh`, `mise.toml`, `mise.optional.toml`, `mise.lock`, `mise.optional.lock`, [`ai/topics/local-tools.md`](../../topics/local-tools.md).
- The tools it invokes are spelled correctly: `mise`, `npm` (with `npm audit`), `cargo` (with `cargo update`), and `cargo audit` — the last pinned as `cargo:cargo-audit` in `mise.optional.toml` and installed on demand through `ci/impl-toolchain.sh install-report-tools`, not assumed as a standing core binary.
- The out-of-scope carve-out still holds: `upgrade-deps` must not touch `version-check.sh`'s mirror list — that is `audit-versioning`'s domain.

## How to report

Group findings into:

1. **Orphan ecosystems** — manifest kinds present in the repo with no `upgrade-deps` handler.
2. **Dangling handlers** — handled ecosystems with no surviving manifest.
3. **Broken references** — a source or tool named in `upgrade-deps` that no longer resolves.

For each finding, cite the manifest path or the `upgrade-deps` line.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) — extend `upgrade-deps`'s handler set, retire the dead handler, or fix the stale reference, then re-run.

---
name: audit-website
description: Verify website/ integrity — VitePress docs generation, wasm inspector bundle, Pages deploy wiring, host-language count sync, ignored outputs, and Kiodoc token CSS drift
allowed-tools: Read, Grep, Glob, Bash
---

# Website audit

Verify the static website contract anchored in [`ai/topics/repo-layout.md`](../../topics/repo-layout.md) (`website/` and `kio-repl-wasm/` entries), [`docs/README.md`](../../../docs/README.md) § Host integrations, [`specs/backends/README.md`](../../../specs/backends/README.md) § Backend pages, and [`README.md`](../../../README.md)'s supported-host-language count.

If `website/` is absent, report that there is nothing to audit yet.

## 1. Website presence and version mirrors

- Verify `website/package.json` and `kio-repl-wasm/Cargo.toml` exist.
- Run `sh ci/checks/repo-lint/version-check.sh`.
- Confirm `ci/checks/repo-lint/version-check.sh` names both manifests.

## 2. Generated docs navigation

- Run `cd website && npm run prepare:docs`.
- Confirm `website/.vitepress/generated/nav.mjs` is derived from `docs/README.md`, not hand-maintained static config.
- Confirm every Markdown file under `docs/tutorials/`, `docs/guides/`, `docs/poc/`, and `docs/hosts/` is represented in `docs/README.md`.
- Confirm generated routes exist under `website/docs/` for the docs index; the section index routes (`tutorials/`, `guides/`, `poc/`, `hosts/`); representative guides, case studies, and host integrations.

## 3. Host-language count sync

- Derive the canonical host-language set from `specs/backends/*.md`, excluding `specs/backends/README.md`.
- Confirm `README.md` mentions the derived count and links to `docs/hosts/`.
- Confirm `website/.vitepress/generated/languages.mjs` derives the same count.
- Confirm `docs/README.md` § Host integrations has one guide for every backend page, or reports an explicit exclusion.

## 4. Deploy workflow

- Read `.github/workflows/pages.yml`.
- Verify it checks out with `persist-credentials: false`, builds the website package's real `npm run build`, uploads `website/.vitepress/dist`, deploys with Pages permissions (`pages: write`, `id-token: write`), uses Pages concurrency with `cancel-in-progress: false`, and pins actions by SHA.
- State that Pages deploy is not a `ci/all.sh` gating job and does not need a paired `ci.yml` job.

## 5. Website E2E orchestrator

- Verify `ci/checks/orchestrators/website-e2e.sh` exists, is executable, supports `-h|--help`, and runs `npm ci`, wasm bundle build through `npm run build`, `npm run audit`, artifact `npm run smoke`, and real-browser `npm run browser:smoke` against that same build.
- Confirm the core mise list, Dockerfile, CI runtime libraries and INSTALL/install-dev-env setup pair the same checksum-pinned Firefox revision on supported Linux/macOS/Windows routes with its exact Playwright driver. A missing browser, sandbox failure, or unsupported runtime is a visible setup failure, never a silent skip or sandbox opt-out.
- Run `sh ci/checks/orchestrators/website-e2e.sh` when the local toolchain has `node`, `npm`, `cargo`, `wasm-pack`, and the managed browser. Report missing runtime evidence explicitly otherwise.
- The smoke must exercise actual page/worker/WASM startup and keyboard input, assert a distinct rendered result plus returned prompt, and reuse state-based bounded waits with diagnostics. Readiness or echoed input alone is not the result oracle; check running negative controls when changing that oracle.
- Verify `npm run browser:history` builds its private Git fixture once and applies both static and rendered navigation checks to that output, without Rust/WASM compilation. Keep `npm run check:history-site` available for focused static iteration, not a second fixture build in the broad gate.

## 6. Ignored generated outputs

Verify `.gitignore` covers `website/node_modules/`, `website/docs/`, `website/.vitepress/cache/`, `website/.vitepress/dist/`, `website/.vitepress/generated/`, `website/.vitepress/public/wasm/`, and `kio-repl-wasm/target/`.

## 7. Kiodoc token CSS drift

- Derive expected classes from `kio-rs/src/tokens.rs` (`TokenKind::css_class`).
- Compare them with selectors in `website/.vitepress/theme/custom.css`.
- A token class emitted by Kiodoc but not styled, or a styled `kio-*` token selector that is not emitted, is a finding.

## 8. Rendered docs leak check

When built output exists under `website/.vitepress/dist`, run `cd website && npm run smoke`.

The smoke must fail on visible Kiodoc fence attributes (`{ignore}`, `{variant=...}`), unresolved intra-doc references, unrewritten `../specs` / `../test-data` links, missing local href targets, missing representative docs pages, missing wasm artifacts, or representative Kio blocks without `language-kio` and `kio-*` spans.

## 9. V1 scope marker

V1 has no run buttons and no Kiodoc Phase 2 run-marker schema. Do not require run-marker checks until that feature lands.

## 10. Landing-page visual review

For landing-page changes, verify the rendered candidate was visually reviewed
by an agent or user under [`ai/topics/readme.md`](../../topics/readme.md)
§ Landing-page visual review. Inspect the review evidence for the changed
content and surrounding layout at relevant viewport sizes. Builds, smoke tests
and DOM assertions are not substitutes for this review; report missing visual
evidence explicitly. This does not require a rendered review of ordinary README
edits or authorize changing protected wording.

Use `npm run browser:capture -- --output NEW_DIRECTORY --page /kio/route.html`
for scoped desktop/narrow rendered captures through the smoke's launcher;
`--full-page` includes below-the-fold content. The same rule applies to other
visible website changes. Do not require an agent screenshot review inside CI,
or treat producing screenshots as having inspected them.

## 11. Editor marketplace links

Read `publisher` and `name` from `tools/vscode-kio/package.json`; compare
marketplace links across README/listing pages, the landing page, install
guidance and any generated installer messages. Follow
[`ai/topics/readme.md`](../../topics/readme.md) § Editor marketplace links:
verify the destinations identify the published extension, and report stale
identities or channels. When network access is unavailable, distinguish local
identity consistency from unverified live destinations. Do not silently edit
protected wording to repair a link.

## How to report

Group findings into:

1. **Broken wiring** — missing manifests, missing CI/deploy files, non-executable scripts, missing version mirrors.
2. **Drift** — docs catalogue drift, host-language count mismatch, stale generated nav/language data.
3. **Rendered output** — Kiodoc syntax leaks, dead repo-relative links, missing token spans, missing wasm artifacts.
4. **Style contract** — token CSS classes missing or stale.

For each finding, cite the file and the violated contract.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

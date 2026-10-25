# `README.md`

Trigger: editing `README.md`, `website/.vitepress/theme/components/Landing.vue`,
or its displayed examples under `website/examples/`.

`README.md` is the public entry point on GitHub. Rules:

- **Keep it consistent with `specs/`, `docs/`, and AGENTS.md § About Kio.** The README's job is to point a passer-by at the right destination; the homes themselves enumerate their contents. § Documentation is a **top-level-home index** — one bullet per important entry-point location in the repo, each with a one-line gloss. The canonical four are `docs/` (pedagogical material), `specs/` (authoritative contracts), `test-data/poc/` (reference modules adopters can copy verbatim), and `ROADMAP.md` (in-flight design threads); add a bullet when a new top-level home worth a passer-by's attention lands. Don't enumerate subarticles here (no `docs/tutorials/`, no `docs/guides/` breakdowns, no specific spec pages) — that duplicates each home's own README.
- **Treat the supported-host-language count as derived data.** If the lead mentions how many host languages Kio supports, derive the number from `specs/backends/*.md` excluding `specs/backends/README.md`, and link the sentence to `docs/hosts/`. The website uses the same host-language set; [`audit-website`](../skills/audit-website/SKILL.md) checks that `README.md`, `docs/README.md`, generated website language data, and the landing page stay in sync.
- **When a `ROADMAP.md` thread settles into a spec, consider adding a Features bullet** — but only for properties a passer-by would care about (type-system guarantees, design pillars, portability claims), not implementation details.
- **Keep it brief.** README is an entry point, not a tutorial. Build instructions, worked examples, and reference material belong in `specs/`, `docs/`, or `test-data/poc/`.

Listing READMEs give a concise capability overview, not an exhaustive inventory
of commands or code actions. Keep capability and availability claims accurate
when the artifact changes; detailed action lists belong in reference material.
The README audit checks these claims without requiring every action to appear.
Apply [People and programs](docs.md#people-and-programs) when explaining
integration responsibilities or tool behavior on these entry pages.

## README and landing-page consistency

When changing a language claim, explanation or example in either entry point,
review the corresponding content in the other, including the canonical sources
of displayed website examples. Keep shared claims and examples consistent with
the current contract. Explain unfamiliar syntax near its first use when readers
need it to understand the example. Each page may use different wording and detail
for its audience; neither needs to repeat the other's complete feature list.
Record the paired check with the change's validation evidence, including any
required counterpart edits awaiting protected-wording approval.

## Landing-page visual review

Render landing-page changes and have an agent or user visually inspect the
candidate before landing. Check the changed content in its surrounding layout,
including wrapping, spacing, clipping and overflow at relevant viewport sizes.
Record the review and any remaining issues with the change's validation evidence.
A successful build, smoke test or DOM assertion does not establish that the
page looks good. This visual gate is for the landing page, not ordinary README
edits; protected exact-wording approval remains a separate requirement.

## Editor marketplace links

Derive extension listing identities from `publisher` and `name` in
`tools/vscode-kio/package.json` and verify them against the actual published
listings. When a publisher, extension name or distribution channel changes,
sweep README/listing pages, the landing page, install guidance and generated
installer messages together. Check that every link still reaches the intended
extension, not merely a successful HTTP response. Keep protected wording
approval intact. The website audit owns this cross-surface link check.

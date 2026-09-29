# `README.md`

Trigger: editing `README.md`.

`README.md` is the public entry point on GitHub. Rules:

- **Keep it consistent with `specs/`, `docs/`, and AGENTS.md § About Kio.** The README's job is to point a passer-by at the right destination; the homes themselves enumerate their contents. § Documentation is a **top-level-home index** — one bullet per important entry-point location in the repo, each with a one-line gloss. The canonical four are `docs/` (pedagogical material), `specs/` (authoritative contracts), `test-data/poc/` (reference modules adopters can copy verbatim), and `ROADMAP.md` (in-flight design threads); add a bullet when a new top-level home worth a passer-by's attention lands. Don't enumerate subarticles here (no `docs/tutorials/`, no `docs/guides/` breakdowns, no specific spec pages) — that duplicates each home's own README.
- **Treat the supported-host-language count as derived data.** If the lead mentions how many host languages Kio supports, derive the number from `specs/backends/*.md` excluding `specs/backends/README.md`, and link the sentence to `docs/hosts/`. The website uses the same host-language set; [`audit-website`](../skills/audit-website/SKILL.md) checks that `README.md`, `docs/README.md`, generated website language data, and the landing page stay in sync.
- **When a `ROADMAP.md` thread settles into a spec, consider adding a Features bullet** — but only for properties a passer-by would care about (type-system guarantees, design pillars, portability claims), not implementation details.
- **Keep it brief.** README is an entry point, not a tutorial. Build instructions, worked examples, and reference material belong in `specs/`, `docs/`, or `test-data/poc/`.

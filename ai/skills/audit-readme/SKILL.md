---
name: audit-readme
description: Verify README.md and the crate/extension listing READMEs — root README specs/§ About Kio consistency, host-count sync, top-level-home index, brevity; listing READMEs anchor-sourced from landing + root README, no metadata/install duplication
allowed-tools: Read, Grep, Glob, Bash
---

# README audit

ai/topics/readme.md commits to README's public-entry rules — keep consistent
with `specs/` and § About Kio, treat the supported-host-language count as
derived data, only add Features bullets for settled-in-spec properties a passer-by
would care about, and keep it brief (entry point, not tutorial). This skill
verifies each.

Read [`ai/topics/readme.md`](../../topics/readme.md), AGENTS.md § About Kio, and [`ai/topics/specs.md`](../../topics/specs.md) before starting.

## 1. Documentation section: top-level-home index

The README's § Documentation is a **top-level-home index** — one bullet per important entry-point location in the repo, each with a one-line gloss. The canonical four are `docs/` (pedagogical material), `specs/` (authoritative contracts), `test-data/poc/` (reference modules adopters can copy verbatim), and `ROADMAP.md` (in-flight design threads). Bullets do not duplicate the per-home README's own subarticle enumeration — readers click through to see what each home contains.

Check:

- Every bullet points at a real top-level directory or file at the repo root.
- The canonical four homes (`docs/`, `specs/`, `test-data/poc/`, `ROADMAP.md`) are all present. If a new top-level home worth a passer-by's attention has landed (a new corpus, tooling directory, etc.) and isn't here, surface as a finding.
- Each bullet's gloss is a single line — no enumeration of subarticles. A bullet like `docs/` followed by `(tutorials, guides, host integrations)` is admissible as part of the gloss; a bullet that *links* into `docs/tutorials/`, `docs/guides/`, etc. as separate sub-items is over-enumeration and is a finding.
- No bullet links directly at a specific `docs/<subdir>/<page>.md`, `specs/<page>.md`, or other deep file — those are the home's job to enumerate. If a specific spec needs surfacing to passers-by, the right move is a Features bullet or a `docs/` guide that quotes it.
- Per-link verification: each bullet's link resolves to an existing path.

## 2. About Kio framing mirror

Compare `README.md`'s lead-paragraph framing of Kio against AGENTS.md § About Kio. The two should agree on:

- The pillars (hosted, dynamically loadable, developer-friendly — or whatever § About Kio currently commits to).
- The formal guarantees (sound, decidable type system; strongly normalizing Kio'; open-world compilation).
- The implementation status (transpiler, host languages enumerated honestly).
- The supported host-language count, when mentioned: derive it from
  `specs/backends/*.md` excluding `specs/backends/README.md`, require a link to
  `docs/hosts/`, and deduplicate with
  `audit-website` when that audit ran in the same pass.

Disagreement is a finding. The README is the one that drifts; AGENTS.md § About Kio is the canonical phrasing.

## 3. Features bullets — settled-in-spec only

ai/topics/readme.md: "When a `ROADMAP.md` thread settles into a spec, consider adding a Features bullet — but only for properties a passer-by would care about (type-system guarantees, design pillars, portability claims), not implementation details."

For each Features bullet:

- Does the property have a spec section pinning it? Check `specs/` for prose that backs the bullet's claim. A bullet whose claim isn't in any spec is over-committing — flag.
- Is the property still a Features-level claim? (e.g. would a passer-by care?) Implementation details that crept in are findings.
- Conversely: are there ROADMAP threads that have since settled into spec and would warrant a new Features bullet? Surface as a suggestion, not a finding — ai/topics/readme.md says "consider" adding, not "must."

## 4. Brevity

The README is "an entry point, not a tutorial." Sweep for:

- Tutorial-shaped sections (walkthroughs, "let's build X" narratives).
- Build instructions (`cargo build` invocations, toolchain setup).
- Reference material that belongs in `specs/` or `docs/`.
- Worked examples beyond a one-screen smoke snippet (if one exists at all).

Each is a finding. The README's job is to point readers at the right downstream doc; longer content lives there.

## 5. Version line

AGENTS.md § Universal rules — Versioning makes the README's version line a mirror point. `audit-versioning` already covers this — surface it here only if `audit-versioning` hasn't been run in the same audit pass, otherwise defer.

## 6. ROADMAP overlap (suggestion-only)

When a ROADMAP entry settles into a spec, the README *may* gain a Features bullet. Walk the recent `ROADMAP.md` history (`git log --since="3 months ago" --diff-filter=D -- ROADMAP.md`) and check for entries that have been removed (signal: thread settled into a spec). For each, check whether the README's Features list has gained or should gain a bullet.

This is judgment-only: ai/topics/readme.md says "consider" adding. Surface candidates rather than asserting.

## 7. Listing READMEs — crate and extension

Two READMEs are **listing pages** for a package registry / marketplace, not repo entry points: `kio-rs/README.md` (the crates.io page for `kio-lang`) and `tools/vscode-kio/README.md` (the VS Code Marketplace / Open VSX page). Both are approval-gated (AGENTS.md § Universal rules) and follow rules the root README does not:

- **Anchor-sourced from the landing page and root README.** The prose draws from the website landing page (`website/.vitepress/theme/components/Landing.vue`, the accessible voice) and the root `README.md` (the expert voice), preferring the landing page's phrasing. Both open on the shared anchor — "Kio is an ultra-portable, embeddable programming language that compiles to host languages — write a package once and run it in any of them" — with the first "Kio" linked to the website. A drifted anchor or a missing opening link is a finding.
- **Only what the platform doesn't already render.** crates.io and the Marketplace surface `homepage`, `repository`, and `license` from the manifest (`Cargo.toml` / `package.json`) in their own chrome. A listing README that repeats them — a "see the website / GitHub" paragraph, a license restatement beyond the conventional one-liner — is redundant; flag it. The README carries the one thing the manifest can't: a real sentence about what the artifact is.
- **Describe the artifact; don't half-explain the language.** A listing README says what *this* package is (the crate is the `kio` compiler / CLI; the extension is editor support) and links out for the rest. An arbitrary subset of language features or guarantees — "why just these three?" — is a finding. Usage the artifact itself genuinely needs is fine: the extension's Features / Requirements / Settings describe the extension, not the language.
- **No install re-hosting on the crate page.** The crate README does not duplicate the website's cross-platform install guide (`curl | sh`); its audience installs via cargo, and `homepage` already points at the full story.

## How to report

Group findings into:

1. **Deep-link violations** — README links directly at a specific `specs/` / `docs/` page instead of the top-level home (per § 1).
2. **About Kio framing drift** — README lead-paragraph disagrees with AGENTS.md § About Kio.
3. **Unbacked Features bullets** — README claims a property no `specs/` section pins.
4. **Brevity violations** — tutorial / build-instructions / reference material in the README.
5. **Suggested Features bullets** — ROADMAP-settled threads not yet reflected in the README (judgment-only).
6. **Listing README violations** — a crate/extension listing page drifts from the anchor, omits the opening homepage link, duplicates platform metadata, half-explains the language, or re-hosts install instructions (per § 7).

For each finding, cite the line and (where applicable) the spec section it should mirror.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

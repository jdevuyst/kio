---
name: audit-no-future-extensions
description: Sweep public artifacts for aspirational content — "future extension" notes, "not yet supported", deferred-feature promises, unbuilt components presented as real — that belongs only in ROADMAP.md
allowed-tools: Read, Grep, Glob, Bash
---

# No-future-extensions audit

Anchor: AGENTS.md § Universal rules — **"Aspirational content lives only in `ROADMAP.md`."** The rule's scope, class catalogue, and admissible exceptions are in [`ai/topics/no-future-extensions.md`](../../topics/no-future-extensions.md); read it before starting. This skill executes the drift sweep so the rule cannot rot as new prose lands.

## Scope

Public artifacts only: `specs/`, `docs/`, README-class pages (`README.md`, `INSTALL.md`, `TESTING.md`, `CONTRIBUTING.md`, subproject READMEs), agent instructions (`AGENTS.md`, `ai/`), and user-facing CLI output strings in `kio-rs/src/` (diagnostic messages, `.with_help(...)` text, `--help` text). Excluded: `ROADMAP.md` (the one legitimate home), internal code comments (governed by [`comments.md`](../../topics/comments.md) — extensibility/guard rationale mentioning the future is fine there), and audit skills that quote the banned phrases as grep patterns (this file included).

## 1. Phrase sweep

```sh
git grep -n -iE 'future extension|not yet supported|future work|\bplanned\b|\beventually\b|\bsomeday\b|for now\b|until then|will be added|future version' \
  -- ':!ROADMAP.md' 'specs/' 'docs/' 'ai/' '*.md'
```

Also reject backend maturity and ad hoc stability labels directly. The exact
`Host API stability: evolving|stable` field is an admissible current
compatibility policy and is validated separately by
`ci/checks/repo-lint/backend-api-stability.sh`:

```sh
git grep -n -iE '(status|maturity|stability):.*(v[0-9]+|experimental|may evolve)|v[0-9]+, may evolve|before .*marked stable' \
  -- 'specs/backends/' 'docs/hosts/' 'ai/'
```

Also sweep user-facing message strings:

```sh
git grep -n -iE '"[^"]*(not yet|future|planned|eventually)[^"]*"' -- 'kio-rs/src/'
```

The second grep over-matches internal strings and comments; keep only hits that reach the user (diagnostics, help/note text, CLI output).

## 2. Triage

Classify every hit as exactly one of:

- **Admissible** — one of the classes in the topic file: a reservation stated
  as a current rejection, a concrete current-caveat banner, the exact Host API
  stability field, a forward-compatibility contract, a policy conditional, or
  a bare pointer at `ROADMAP.md`. Cite the class. Backend maturity and ad hoc
  stability labels remain findings, not caveats.
- **Homonym false positive** — the phrase means something else (the argument-slot planner's "planned slot" in `specs/language.md`, "not yet sealed" describing runtime `kio sig` state, "not yet in scope" describing declaration-order semantics, install advice about "future shells").
- **Finding** — a deferred-feature note, an unbuilt component presented as real (a consumer list naming a tool that doesn't exist — verify existence in the tree, don't trust the prose), or dev-process narration.

## 3. Unbuilt-component check

For each named tool, pass, pipeline stage, or consumer in a spec's or doc's structural claims (consumer lists, architecture descriptions), verify it exists in the tree. A component the prose presents as operating today but which no tracked file implements is a finding even when no banned phrase appears.

## How to report

List findings with file:line, the quoted sentence, the class it violates, and a suggested present-tense rewrite. Deleting a promise must not delete normative content: when the sentence also carried a current rule (what is rejected, what the supported route is), the rewrite keeps that rule. Report admissible-class counts summarily; report every finding individually.

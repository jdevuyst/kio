---
name: audit-kio-guide
description: Keep ai/topics/kio-authoring.md sane — pointers resolve, it routes rather than re-teaches, claims are backed (no bug-workarounds), and the docs/README.md guide catalog stays current
allowed-tools: Read, Grep, Glob, Bash
---

# Kio-authoring guide audit

[`ai/topics/kio-authoring.md`](../../topics/kio-authoring.md) is the agent navigator for writing surface Kio and inspecting the Kio' it lowers to. By design it **routes** to the public catalog (`docs/README.md`), the formal specs, and agent-only topics, while holding only agent-specific deltas + the Kio'-inspection quick reference — it does not duplicate language content. That design is what keeps this audit buildable: there is no second prose guide to diff for semantic equivalence (which would be un-mechanizable, the same reason per-backend impossibility claims need manual discharge — see [`AGENTS.md` § Per-backend limitations]). This audit is structural checks plus a heuristic sweep with forced discharge.

Read `ai/topics/kio-authoring.md` and `AGENTS.md`'s trigger table before starting.

Checks 1–3 are mechanical (suitable for continuous CI enforcement). Checks 4–6 are heuristic signals that hand judgment to a per-item discharge.

## 1. Pointers resolve (structural)

Every linked file and every `specs/… § Section` citation in the navigator must resolve.

Markdown links (resolve relative to `ai/topics/`):

```sh
cd ai/topics
grep -oE '\]\(([^)]+\.md)\)' kio-authoring.md | sed -E 's/^\]\(([^)]+)\)$/\1/' \
  | sort -u | while read -r link; do
    f="${link%%#*}"
    [ -e "$f" ] || echo "kio-authoring.md: broken link → $link"
  done
cd - >/dev/null
```

Plain-text spec-section citations (the prose + the "Surface | In Kio'" reading key cite `specs/<file>.md § Heading` inside backtick code spans, so strip backticks before scanning):

```sh
sed 's/`//g' ai/topics/kio-authoring.md | grep -oE 'specs/[A-Za-z0-9/-]+\.md § [^]|]+' | sort -u | while read -r ref; do
  file="${ref%% § *}"
  cite="${ref##* § }"
  [ -f "$file" ] || { echo "missing spec file: $file  (from: $ref)"; continue; }
  sed 's/`//g' "$file" | awk -v cite="$cite" '
    BEGIN { lc = tolower(cite) }
    /^#/ { t = $0; sub(/^#+[ \t]+/, "", t); lt = tolower(t)
           if (lt != "" && (index(lc, lt) == 1 || index(lt, lc) == 1)) { found = 1; exit } }
    END { exit found ? 0 : 1 }' ||
    echo "spec heading not found: $file § $cite"
done
```

(The spec file is scanned backtick-stripped so a heading like ``the `newtype` primitive`` still matches, and headings are compared as fixed strings — prefix in either direction — because a citation can run into trailing prose or punctuation and may contain regex metacharacters.)

A fuzzy heading miss can be a wording drift rather than a true break — glance at the spec's headings before reporting, and prefer fixing the citation to the spec's current section title.

## 2. `surface-forms.md` stays linked (structural)

When the `.kio` trigger repointed from `surface-forms.md` to `kio-authoring.md`, `surface-forms.md` must remain referenced from **both** `AGENTS.md` (or `audit-agents-md` § 3 flags it as an orphan topic) and the navigator (so the lowering discipline is one click from the entry point).

```sh
grep -qF 'ai/topics/surface-forms.md' AGENTS.md || echo "surface-forms.md no longer referenced from AGENTS.md (orphan risk)"
grep -qF 'surface-forms.md' ai/topics/kio-authoring.md || echo "kio-authoring.md no longer links surface-forms.md"
```

## 3. The Kio'-inspection command is real (structural smoke)

The navigator pins `kio build kio-prime` against a `target kio-prime { out "…"; }` block. The `kio-prime` target id must still be a recognised build backend (it's `js` / `rust` / `kio-prime`, not the cargo `prime` *feature*).

```sh
grep -qE '"kio-prime"\s*=>' kio-rs/src/cmd/build.rs \
  || echo "kio-prime target id not found in dispatch_target — the pinned 'kio build kio-prime' is stale"
```

If the dump invocation, the `target kio-prime` block shape, or the reading-key form names have drifted, the inspection section is teaching a dead command — fix it against the current backend.

## 4. Routes, not re-teaches (heuristic + discharge)

The whole value of the navigator is that it points; the failure mode is an agent pasting language explanations in, re-creating the duplication the design avoids. Signal: the § Writing surface Kio table rows all link out. Flag any sizeable link-free prose block inside the authoring/routing sections.

```sh
# routing-table rows missing an outbound link are the first thing to inspect
grep -nE '^\| ' ai/topics/kio-authoring.md | grep -v '](' || true
```

Discharge each flagged block: **is this routing/agent-delta/inspection content, or has someone pasted in language teaching that now duplicates `docs/guides/`?** If the latter, move it to the relevant guide and replace it with a pointer. (The § Agent deltas, § Inspecting the Kio', and § Improving this guide sections legitimately carry prose — they are the agent-specific delta, not duplicated language content. Judge by *what* the prose does, not its length.)

## 5. Claims are backed; no bug-workarounds (heuristic + discharge)

A guide entry that makes a behavioural claim should cite a spec section or a golden. And nothing in the guide may be an emitter-bug workaround disguised as an idiom — that violates [`AGENTS.md` § Bugs surface; never hide them] and [`AGENTS.md` § Goldens demonstrate behavior, not workarounds].

```sh
# workaround smell — each hit is a discharge item, not an automatic fail
grep -niE "avoid|doesn't work|does not work|the emitter|workaround|instead of .* because|chokes|breaks the build" \
  ai/topics/kio-authoring.md docs/guides/*.md || true
```

Discharge each hit: **a real idiom (then it needs a spec/golden citation), or a hidden bug-workaround (then it's a bug to surface and fix, and the entry comes out)?** This is the check that stops the guide rotting into folklore.

## 6. Public guide catalog delegation (heuristic + discharge)

The navigator delegates public guide discovery to `docs/README.md`, so it should link the catalog instead of mirroring every guide. The catalog must list every checked-in guide:

```sh
grep -qF '../../docs/README.md' ai/topics/kio-authoring.md \
  || echo "kio-authoring.md no longer links docs/README.md as the public guide catalog"

for g in docs/guides/*.md; do
  base="$(basename "$g")"
  grep -qF "guides/$base" docs/README.md || echo "not listed in docs/README.md: $g"
done
```

Discharge each: **a real catalog omission** (add the guide to `docs/README.md`) or **an intentionally hidden/internal guide** (rare; explain why public readers should not see it). If `kio-authoring.md` starts listing many individual guides again, collapse that back to the catalog plus only the agent-specific deltas that truly belong here.

## Reporting

Lead with checks 1–3 (mechanical pass/fail). Then the discharge items from 4–6, each with your verdict and the concrete fix (sharpen a pointer / move prose to a guide / file a bug / add a routing row). If invoked with a fix directive, see [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

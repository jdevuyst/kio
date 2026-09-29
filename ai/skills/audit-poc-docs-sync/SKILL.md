---
name: audit-poc-docs-sync
description: Verify case studies stay paired with their POCs — backed, docs ⊆ POC, headline coverage, and usable library-adoption instructions
allowed-tools: Read, Grep, Glob, Bash
---

# POC ⇄ case-study sync audit

`docs/poc/<x>.md` case studies and `test-data/poc/<x>/` packages are **pairs**: each case study is a reference-grade read-through bounded by its backing package. The contract — `docs ⊆ POC`, backed, and covered — is documented in [`ai/topics/docs.md`](../../topics/docs.md) § Keeping a case study paired with its POC. This skill verifies the pair stays in sync. The POC is the ground truth (it is `kio check` + `kio test` + per-backend-`main`-run checked on every CI run); the prose is derivative, so every divergence is a docs bug unless the POC itself is wrong.

Read [`ai/topics/docs.md`](../../topics/docs.md) and [`test-data/poc/README.md`](../../../test-data/poc/README.md) before starting.

## 1. Every case study has a backing POC

Each `docs/poc/<x>.md` must have a backing `test-data/poc/<x>/workdir/`. A case study with no POC is an orphan — the prose has no ground truth to be derived from.

```sh
for page in docs/poc/*.md; do
  topic=$(basename "$page" .md)
  test -d "test-data/poc/$topic/workdir" || printf 'ORPHAN case study (no test-data/poc/%s/): %s\n' "$topic" "$page"
done
```

The converse — a POC with no case study — is **not** a finding here: not every POC needs a reference-grade case study (the corpus has `list`, `vec`, `dict`, … that are covered by [`audit-corpus`](../audit-corpus/SKILL.md)'s POC library contract without a `docs/poc/` page). Only the pages that exist must each be backed.

## 2. Backing-POC link present and resolves

A case study must link to its backing package so a reader can reach the ground truth. Check that each `docs/poc/<x>.md` references `test-data/poc/<x>/` (the package root or a file under its `workdir/`), and that every `../../test-data/poc/...` and `../../specs/...` link target it names actually exists on disk.

```sh
python3 - <<'PY'
from pathlib import Path
import re, sys
fail = []
for page in sorted(Path("docs/poc").glob("*.md")):
    topic = page.stem
    text = page.read_text()
    if f"test-data/poc/{topic}/" not in text:
        fail.append(f"{page}: no link to backing test-data/poc/{topic}/")
    for m in re.finditer(r"\]\((\.\./\.\./[^)#]+)", text):
        target = (page.parent / m.group(1)).resolve()
        if not target.exists():
            fail.append(f"{page}: dangling link target {m.group(1)}")
    # intra-docs links (siblings, guides, tutorials)
    for m in re.finditer(r"\]\((?!\.\./\.\.|https?:|#)([^)#]+\.md)", text):
        target = (page.parent / m.group(1)).resolve()
        if not target.exists():
            fail.append(f"{page}: dangling docs link {m.group(1)}")
for f in fail:
    print(f)
sys.exit(1 if fail else 0)
PY
```

A dangling spec or POC link is a finding (the case study points a reader at content that does not exist). A dangling sibling-docs link is a finding too — a moved or deleted guide must not leave a hanging reference.

## 3. `docs ⊆ POC` — nothing documented is absent from the POC

This is the load-bearing check. Every Kio identifier a case study presents as belonging to the package — types, functions, elaborators, operators, labels — must exist in the backing package's `workdir/` sources. The case study is a read-through of the POC, not a place to invent or rename API.

The high-signal classes to verify:

- **Validated `kio` fences.** A case study's `{@harness}`-validated snippets are the strongest `⊆` evidence: if `kio doc check` passes, the snippet compiles against a harness that supplies the POC's host shape, so the surface it uses is real. Run `kio doc check docs/poc/<x>.md` and treat any snippet failure as a finding (either the snippet drifted from the POC, or the POC changed under it).
- **Named declarations in prose and `text` fences.** A case study also quotes `equiv` laws and type / fn signatures in non-validated `text` fences. Each top-level declaration name those quote (`fn <name>`, `type <name>`, `newtype <name>`, `equiv <name>`, `op` pattern, `pub elab <name>`) must appear in the backing package. Spot-check by extracting the declaration names the page quotes and grepping the package:

```sh
# For docs/poc/<x>.md, every `equiv <name>` quoted in the page must exist in the POC.
page=docs/poc/<x>.md
topic=<x>
grep -oE '^equiv [A-Za-z_][A-Za-z0-9_]*' "$page" | sort -u | while read -r _ name; do
  grep -rqE "^equiv $name\\b" "test-data/poc/$topic/workdir" \
    || printf 'docs ⊄ POC: equiv %s quoted in %s not found in package\n' "$name" "$page"
done

# Likewise top-level fn / type / newtype / elab names the page quotes inside kio/text fences.
grep -oE '\b(fn|type|newtype|pub elab) [A-Za-z_][A-Za-z0-9_]*' "$page" | sort -u | while read -r kind name; do
  grep -rqE "(^|\\bpub )$kind $name\\b" "test-data/poc/$topic/workdir" \
    || printf 'docs ⊄ POC: %s %s quoted in %s not found in package\n' "$kind" "$name" "$page"
done
```

A name the case study presents as the package's that the package does not declare is a `docs ⊄ POC` violation. (Filter false positives: a name introduced only as an *illustrative* generic — `A`, `B`, `T` type binders, a lambda parameter — is not a package declaration; the grep above keys on declaration leads, not every identifier.) When a violation is real, the fix is to correct the prose to match the POC, never to reshape the POC to match the prose.

- **`equiv` laws quoted verbatim.** When a case study shows an `equiv` block in a `text` fence claiming it is the package's law, the block must match the package's source. A reworded or simplified law is a `⊄` violation — it misrepresents what `kio test` actually discharges.

## 4. The POC's headline public surface is covered

A case study should walk the package's whole public API and operator DSL, not a convenient subset. A `pub` item the package exports but the case study never names is a coverage gap (a "headline omission"). Pure host-boundary scaffolding (`testapi` modules) and shared private utilities are out of scope; the check is over the package's own copyable library `pub` surface.

```sh
# Every pub top-level item in the package's headline root library modules
# should be named somewhere in the case study. Exclude nested demos,
# materialized dependency roots, host scaffolding (`testapi`), and shared
# elaborator-internal utility modules (`elaborator_util`, the `*_util`
# convention) — those are plumbing other modules import across the boundary,
# not the package's headline surface.
page=docs/poc/<x>.md
topic=<x>
python3 - <<'PY'
from pathlib import Path
import re

page = Path("docs/poc/<x>.md")
topic = "<x>"
workdir = Path("test-data/poc") / topic / "workdir"
text = page.read_text()
dependency_roots = {dep.parent / dep.name.removesuffix(".dep.kio") for dep in workdir.rglob("*.dep.kio")}

for src in sorted(workdir.rglob("*.kio")):
    if src.name.endswith((".pkg.kio", ".sig.kio", ".dep.kio")):
        continue
    if "/demo/" in src.as_posix():
        continue
    if src.name == "testapi.kio" or "/testapi/" in src.as_posix():
        continue
    if src.name in {"elaborator_util.kio"} or src.name.endswith("_util.kio"):
        continue
    if any(src.is_relative_to(root) for root in dependency_roots):
        continue
    for match in re.finditer(r"^pub (fn|type|newtype|op|elab|labels) ([A-Za-z_][A-Za-z0-9_]*)", src.read_text(), flags=re.M):
        kind, name = match.groups()
        if name not in text:
            print(f"headline omission: pub {kind} {name} from {src} not mentioned in {page}")
PY
```

A handful of omissions of **minor helpers** is acceptable (the contract says "headline" surface): a form-builder's internal constructors (`rep_*`, `show_*`), a checked-impl helper (`*_accepts`, `*_or_raw`), or a view-tail type are plumbing, not headline. The salient surface is what a reader comes for — a package's central types and its primary combinators / elaborator bang-call names. The optics case study omitting a private display helper is fine; omitting `compose_lens`, or the elab case study omitting `fit!`, would be a real finding. Judge by salience, not by raw count.

## 5. No reshaped-source tells

A case study must not present POC source that has been reshaped to read more nicely than the package — adding type annotations the POC elides, splitting one polymorphic declaration into monomorphic ones, renaming for narrative flow. Compare a sampled snippet against its source-of-record in the package: a `kio` fence claiming to be "the library's `view`" must match `optics.kio`'s `view` modulo whitespace. Divergence is a finding — fix the prose, or (if the POC genuinely reads worse than it should) fix the POC, never silently let the two disagree.

## 6. Usable library adoption

For each case study presenting a reusable library, follow its adoption example
or its direct link to the library's example in the using-libraries guide. Check
the adoption contract in [`ai/topics/docs.md`](../../topics/docs.md) § Keeping a
case study paired with its POC:

- Resolve the package-manifest path against the repository and verify the
  consumer filename/alias, required host bindings, and imports against the
  actual public surface. Consumer configuration is not a library-source quote.
- Verify that the reader is told where to run `kio dep fetch` and can reach the
  explanation of locks and materialized sources. Missing setup must not be
  left for the reader to reverse-engineer from a test runner.
- Distinguish syntax validation, local package/materialization evidence, and
  an actual fetch at the published ref. Report missing or failing evidence
  accurately; do not turn a local fixture pass into a remote-fetch claim.

## How to report

Group findings into:

1. **Orphan case study** — `docs/poc/<x>.md` with no `test-data/poc/<x>/workdir/`.
2. **Dangling link** — a spec, POC, or sibling-docs link target that does not exist.
3. **`docs ⊄ POC`** — a declaration name, signature, or `equiv` law the case study presents as the package's that is absent from or reworded relative to the package. The most severe class; cite the name and both locations.
4. **Headline omission** — a salient `pub` item the package exports that the case study never covers.
5. **Reshaped source** — a snippet that diverges from its package source-of-record.
6. **Unusable adoption instructions** — absent or incorrect dependency examples, package paths, host bindings, imports, fetch steps, or supporting links.

For each finding, cite the case study page, the backing package path, and the declaration / snippet / link involved.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). The fix for a `docs ⊄ POC` or reshaped-source finding is almost always to correct the prose to match the POC; reshaping the POC to match the prose is forbidden unless the POC is independently wrong (a real bug, surfaced and fixed per the universal "bugs surface, never hide" rule, with the case study updated in the same change).

---
name: audit-host-docs-snippets
description: Verify every docs/hosts page's host-language fences stay covered by the host-docs-snippets gate — no page or fence silently escapes, exemptions are script-documented, and the per-language compile commands stay truthful
allowed-tools: Read, Grep, Glob, Bash
---

# Host-docs snippet-coverage audit

[`ai/topics/docs.md`](../../topics/docs.md) names
[`ci/checks/orchestrators/host-docs-snippets.sh`](../../../ci/checks/orchestrators/host-docs-snippets.sh)
as the owner of host-language fence validity on `docs/hosts/<lang>.md` pages:
each page's own example package is built and every host fence is compiled
against the freshly-built facade
([`specs/backends/README.md`](../../../specs/backends/README.md) § The package
facade is the surface being validated). This skill is the meta-check over
that mechanical gate — it verifies the gate's *coverage* cannot silently
shrink, which the gate itself cannot notice.

Read the script, the `ai/topics/docs.md` paragraph,
[`ai/topics/specs.md`](../../topics/specs.md) § What goes into a backend spec
item 2, and [`ai/topics/emit.md`](../../topics/emit.md) § Stable host release
and capability floor first. The page set is `docs/hosts/*.md`; the backend set
is `specs/backends/*.md` minus the README — read both at run time.

## 1. Every page is covered

Every `docs/hosts/<lang>.md` must be processed by the script (and every
shipping backend must have a page). A page the script skips entirely, or a
backend whose page is missing, is a finding. The script's own no-silent-skip
rule — a page contributing **zero** validated host fences fails the run —
must still be present in the script; its removal is a finding.

## 2. Every fence is validated or exempt-with-reason

For each page, enumerate its host-language fences and confirm each is either
compiled by the script or covered by an exemption **documented in the script
header** (the standing one: structural-shape illustration fences whose mint
names have no counterpart in the tiny example package compile against a
self-contained harness instead of the real artifact). An undocumented
exemption is a finding. A fence demoted to a `text`/unlabeled block, or
deleted, since the last audit — check `git log -p` for the pages — is the
evasion this audit exists to catch: flag it unless the page's prose genuinely
stopped teaching that snippet.

## 3. Compile commands stay truthful

The script's per-language compile steps must match the real toolchain set the
per-backend runners use (same compilers, resolved the same mise-owned way)
and must exercise the page's snippets against the **current** facade shape.
If a backend's loading protocol changed (new file layout, new marker, new
factory spelling) and the script still compiles the old shape, that is a
finding even while the gate passes.

Distinguish the backend spec's concrete ecosystem-native public language floor
from the exact repository toolchain resolution. Direct tools are declared in
mise configuration and locked where supported; an installer-managed compiler
or runtime has its delegated version pinned explicitly in that configuration
or its visible install hook. The script runs through that resolved toolchain,
but its manifest, module directive, release/language-mode flag, or checker
target must not contradict the declared floor. Every ordinary stable source
pragma, manifest/edition declaration, or language mode needed by a gating
capability is named by that floor and carried by the emitted artifact or host
build; an unstable or experiment-only opt-in is not a substitute.

For a backend admitted or explicitly re-baselined under the current policy,
verify that a real floor toolchain or a mode that truthfully enforces the floor
separately compiles the emitted artifact and the **complete** host-fence set,
including self-contained illustrations, with exactly the floor's named
non-default stable pragmas, declarations, and modes in force and no preview or
unstable experiment enabled. Passing under a newer compiler without enforcing
the declared floor and those modes is not floor evidence. If the repository
compiler has no mode that enforces both the floor's language and
standard-library surface, the separate floor lane owns that proof and the
snippet gate must not claim it for itself. For a frozen legacy floor, report a
direct contradiction between the script's emitted features or version modes
and the declared floor, but do not require a new historical floor lane absent
authority. Repository-resolution updates governed by
[`upgrade-deps`](../upgrade-deps/SKILL.md) do not silently move the public
floor.

Every actual Rust, Go, Java, Swift, or Haskell compiler invocation in the
script—including emitted-package prebuilds and self-contained illustration
harnesses—must go through its shared `run_native_compiler` boundary. A direct
`rustc`, compiler-producing `go`, `javac`, `swiftc`, or `ghc` invocation is a
finding because it bypasses repository-wide compiler admission. Review helper
functions and newly added language branches manually; do not rely on a
syntax-only grep to prove the call graph.

## 4. Toolchain-absent behavior matches the shard model

The script skips a page's compile step when that host toolchain is absent
(mirroring CI's implementation-shard model) while still building the Kio
package. Confirm the always-on set (js / ts / python / rust) is never
skipped, and that the skip prints loudly rather than passing silently.

## How to report

Group findings into:

1. **Uncovered page / missing page** — a docs/hosts page outside the gate, a
   backend without a page, or a weakened no-silent-skip rule (§ 1).
2. **Escaped fence** — a fence neither validated nor script-documented as
   exempt, or a fence demoted/deleted to dodge the gate (§ 2).
3. **Untruthful compile step** — a per-language step drifted from the real
   toolchain, facade shape, or declared public language floor, including an
   unnamed required stable source/manifest language mode or an applicable
   admission/re-baseline floor proof that omits that mode or relies only on a
   newer compiler without enforcing the floor (§ 3).
4. **Skip-model drift** — the toolchain-absent behavior diverged from the
   shard model or went silent (§ 4).

For each finding, cite the page, the fence (line range), or the script line.

**Default: report only.** If invoked with a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

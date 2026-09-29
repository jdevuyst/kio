---
name: audit-backends-shape
description: Verify each backend page follows the 9-section structure and references every applicable shared README contract rather than re-stating it
allowed-tools: Read, Grep, Glob, Bash
---

# Backend page shape audit

[`ai/topics/specs.md`](../../topics/specs.md) § What goes into a backend spec prescribes a 9-section structure for each `specs/backends/<lang>.md`. This skill verifies the structure and the cross-cutting-properties-go-in-README rule.

Read [`ai/topics/specs.md`](../../topics/specs.md) § What goes into a backend
spec, [`ai/topics/emit.md`](../../topics/emit.md) §§ Stable host release and
capability floor; Decision record and propagation gate, and
`specs/backends/README.md` before starting.

## 1. Required sections

For each `specs/backends/*.md` file other than `README.md`, check the page covers (in some order):

1. **Family declaration, API stability, and caveat parity** — declares
   `Family: <name>` from the README table; carries exactly one `Host API
   stability: evolving|stable` field immediately below the title, mirrored by
   the host guide and followed in both files by an adjacent link to the shared
   Host API stability contract; carries no backend-specific version, maturity
   tier, or other stability label; and has no banner unless a concrete known
   caveat exists. The required compatibility field never weakens caveat or
   completeness rules. Any caveat banner names the actual breakage and host
   action and is mirrored in `docs/hosts/<lang>.md`.
2. **Language version** — a concrete ecosystem-native public floor, with every
   stable gating feature and required ordinary stable source pragma,
   manifest/edition declaration, or language mode named. It is not a floating
   "latest" target or the exact repository toolchain resolution. For a backend
   admitted or a floor re-baselined under the current policy, check the owning
   decision record: the chosen line was maintained and GA/final at selection
   time; each gating capability belonged to its stable surface, was usable
   through stable defaults or the named stable artifact/build modes, worked
   across the emitted facade's package/module boundary, and was present in
   every runtime/toolchain class the backend contract admits. No preview,
   experimental, release-candidate, nightly-only, unstable feature-gated, or
   experiment-only opt-in capability carries the contract. That decision
   record also proves the floor by compiling the emitted artifact and complete
   host-fence set with exactly the named non-default stable modes. Do not treat
   a still-frozen older floor as a violation merely because another stable
   release now exists. For a legacy floor not admitted or re-baselined under
   this policy, report a direct contradiction between an emitted feature and
   the declared floor, but do not demand a newly invented floor-toolchain lane
   absent authority for that work.
3. **Output layout** — files `kio build <id>` writes, which is the entry point, which are internal — **and the package-namespace derivation** (`specs/backends/README.md` § The package facade § The package namespace): file and namespace names derive from the package name, with the `namespace` key as the override. A fixed, package-independent top-level name presented as the current layout is a finding.
4. **Loading protocol** — steps a host performs to bring the package online.
5. **Package API** — exported items + generic intrinsic API for constructing / inspecting opaque structural values. Documents methods, not value shapes. Must present the **branded typed facade** — handle / `<Handle>Host` / factory per § The package facade § Branded naming; a statically-typed backend page documenting string-keyed lookup or host-side casts as the current surface is a finding.
6. **Host record contract** — how `host fn` declarations map to host-supplied callables. Spelled out so users don't read a value-layout table as a host-item checklist.
7. **FFI surface** — atomic types, structural and nominal types, carve-outs.
8. **Item naming** — name-mangling rules at the FFI surface, or explicit "no mangling needed" — **and the namespace derivation** (the default from the package name plus the backend's keyword / unimportable-name mangles).
9. **Worked example** — minimal end-to-end snippet.

Flag pages missing any section.

## 2. Cross-cutting properties go in README

`ai/topics/specs.md`: "Cross-backend properties live in
`specs/backends/README.md`, not on each per-backend page." Build the candidate
set from every normative shared heading and every `**Applies when:**` clause in
the README. Determine applicability for each backend and distinct occurrence
from the semantic predicate and backend evidence, never from family membership
alone. The current universal examples are:

- Host API stability
- Synchronous calling convention
- Type erasure at the FFI
- Open-world property
- Package isolation
- Exception propagation
- Well-foundedness inheritance
- Behavioral additivity

For each backend page, verify it references every applicable shared rule and
documents only its current host realization or concrete caveat. A page that
redefines a shared property, omits an applicable reference, or contains a
reusable law that should be shared is a finding. A non-applicable rule needs
the failed-predicate proof in the decision record; family membership is not
that proof.

The exact Host API stability field plus its adjacent README link discharges
that universal rule for a backend page: the field records the local value and
the link delegates all shared meaning. Do not require or permit a local
restatement of the compatibility policy.

## 3. Out-of-scope material

When a backend consumes sealed signature history, its `## Deprecated host
items` realization is required host-facing material. It names the native
deprecation spelling, proves new hosts omit removed function implementations
and type selections, states that retained material is absent from loader and
runtime/package dispatch, and names the concrete caveat when optional
retention is impossible. It also covers the source edit when incompatible
declaration epochs make the shared planner omit an affected retained root and
type closure. A generic claim that removals are tolerated is not enough.

[`ai/topics/specs.md`](../../topics/specs.md) § What goes into a backend spec lists what should NOT appear in a backend page:

- Per-target build-block keys (those live in `specs/package.md` § Build target files).
- Codegen internals.
- Convenience shortcuts the implementation emits but hosts shouldn't rely on (e.g. JS's `globalThis.main`).

Grep each backend page for these and flag.

## 4. Publication header

Run `sh ci/checks/repo-lint/backend-api-stability.sh`. Reject a missing,
duplicate, misplaced, invalid, or mismatched Host API stability field. The only
valid values are `evolving` and `stable`; every new backend starts `evolving`,
and a transition is paired in spec and guide with the explicit transition
record required by `ai/topics/specs.md` § What goes into a backend spec. A
spec/guide rename is removal of the old backend ID plus addition of the new ID,
not a status transfer; reject a stable rename without an earlier published
demotion, a new renamed ID that does not start `evolving`, or a field added
after its backend pages except in the one initial policy rollout.

Reject backend-specific versions, maturity labels, and ad hoc stability labels
(`v1`, `experimental`, `may evolve`, “before stable”) in both the backend spec
and host guide. The exact Host API stability field is compatibility policy, not
a maturity label. Shipping backends are presumed to implement the full Kio
contract at either value; neither value can weaken it.

If a concrete known caveat exists, check that the backend spec and host guide carry matching top banners naming the same breakage and required host action. A banner does not excuse a fixable implementation gap, and a vague warning is a finding.

## 5. New backend pages

If a new `specs/backends/*.md` has landed (`git log --since="3 months ago" --diff-filter=A -- specs/backends/`):

- Confirm it starts by linking `specs/backends/README.md`.
- Confirm its backend spec and host guide both start with the exact matching
  `Host API stability: evolving` field and adjacent shared-contract link.
  Admission never starts at `stable`.
- Confirm it only spells out backend-specific instantiations of cross-cutting properties.
- Confirm the add-backend decision record enumerates the new backend against
  every universal and semantic-predicate shared rule, closes every applicable
  row with conformance and complete-workflow evidence, and the page references
  those current rules without copying them.
- Confirm the release decision distinguishes the concrete frozen public floor
  from the exact repository toolchain resolution, declares directly managed
  tools in mise configuration and locks them where supported, explicitly pins
  delegated sub-toolchains, uses only capabilities in the maintained GA/final
  stable surface through stable defaults or named ordinary stable
  source/manifest language modes across every admitted runtime/toolchain class,
  and carries both complete artifact-plus-host-fence floor compilation with
  those exact stable settings and exact-resolution runtime evidence.
- Confirm ai/topics/specs.md has been updated to name the new page.

## 6. Namespace conformance sweep

The facade is package-branded (`specs/backends/README.md` § The package facade). Grep every backend page for legacy fixed singletons used as **current normative text** — `kio_pkg`, `KioPkg`, `KioPackage`, `createPackage`:

```
grep -nE 'kio_pkg|KioPkg|KioPackage|createPackage' specs/backends/*.md
```

A page presenting a fixed, package-independent name as the current surface is a finding. Historical notes, deprecation narratives, and explicit contrast mentions ("not a fixed `createPackage`") are exempt — read the hit in context.

## How to report

Group findings into:

1. **Missing sections** — backend pages where one of the 9 required sections is absent or under-developed.
2. **Cross-cutting restatements** — pages that re-define a property instead of referencing the README.
3. **Applicability gaps** — missing backend/occurrence rows, family-inferred
   results, applicable shared rules not referenced, and reusable local laws not
   lifted to their semantic scope.
4. **Out-of-scope content** — material that should live elsewhere.
5. **Publication-header violations** — missing, duplicate, misplaced, invalid,
   or mismatched Host API stability fields; non-`evolving` new-backend defaults;
   unrecorded or unpaired transitions; backend versions, maturity/ad hoc
   stability labels, vague banners; and concrete caveats missing a matching
   spec/host-guide banner.
6. **New page bookkeeping** — recently-added pages whose registration in ai/topics/specs.md is missing or whose semantic-applicability discharge is incomplete.
7. **Namespace nonconformance** — a fixed top-level name or an unbranded/stringly surface presented as current contract (§ 3/§ 5/§ 8 strengthenings and the § 6 sweep).
8. **Language-floor violations** — a floating target or repository-toolchain
   resolution presented as the public floor, preview/experimental/unstable
   gating feature, an unnamed required stable source/manifest language mode,
   missing applicable admission or re-baseline evidence, conflated floor and
   repository resolution, an unpinned delegated toolchain, a capability
   missing from an admitted runtime class, or an applicable floor proof that
   omits the artifact, host fences, or the floor's named stable modes.

For each finding, cite the backend page and the missing/extra section.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

---
name: audit-package-coexistence
description: Verify package coexistence stays tested — distinct maximal-collision packages and one package built under two configured namespaces cover every backend, and each runner genuinely hosts two live artifacts
allowed-tools: Read, Grep, Glob, Bash
---

# Package-coexistence audit

[`specs/backends/README.md`](../../../specs/backends/README.md) § The package
facade § Coexistence guarantees that two packages whose namespaces differ load
into one host program without symbol conflict — the property that follows from
the package-namespace rule's totality, complementing § 4 Package isolation's
instance-*state* guarantee. The executable witnesses are
[`test-data/goldens/00_success/ffi_two_packages_coexist/`](../../../test-data/goldens/00_success/ffi_two_packages_coexist/)
and
[`test-data/goldens/00_success/ffi_same_package_namespaces_coexist/`](../../../test-data/goldens/00_success/ffi_same_package_namespaces_coexist/),
both driven through the `coexist` runner protocol
([`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md)
§ The coexist protocol; the corpus contract is
[`TESTING.md`](../../../TESTING.md) § Decision tree — Multi-package
coexistence). This skill verifies neither half of the guarantee can silently
decay.

Read the three anchors above before starting. The shipping-backend set is
`specs/backends/*.md` minus the README — read it at run time; do not hardcode.

## 1. Every backend is declared

Both packages under `ffi_two_packages_coexist/workdir/` and both configured
builds under `ffi_same_package_namespaces_coexist/workdir/` must declare a
`target <lang>` block for **every** shipping backend. Each witness's two target
sets must be identical. A backend missing from either side of either witness is
a finding (the per-backend opt-out these goldens exist to forbid); a new
backend absent from both is the `add-backend` Step-5 gap.

## 2. Each fixture preserves its distinct claim

- In `ffi_two_packages_coexist`, the two packages remain **identical up to
  their package names and output strings**: same module paths, export leaves,
  host-fn leaves, and structural shapes. Diff the two `workdir/pkg_*` trees
  modulo the package name; a divergence that removes a collidable name weakens
  the maximal-collision witness.
- In `ffi_same_package_namespaces_coexist`, the two trees remain the same
  source package: byte-identical Kio modules and the same package name. Their
  manifests differ only where needed to configure two distinct effective
  artifact namespaces. A source, package-name, protocol, or facade-shape
  divergence turns this into another two-package test and is a finding.

## 3. Each runner's coexist arm is genuine

For each runner bin, read its `coexist` arm and confirm it hosts **two live
artifacts** for both witnesses: both instantiated through their published
factories, each behind its own host, with calls interleaved (first → second →
first) so the pinned stdout proves simultaneous liveness. For the same-package
witness, confirm the two ordered descriptors carry the same package name and
different configured namespaces. Findings: a second artifact built but never
instantiated, one instance reused for both, output replayed rather than
produced, a namespace ignored, or an arm routing to a single-artifact path.
`kio-test-runner-dyn-load-prime` is exempt (documented in the runner README).

## 4. Spec ↔ witness agreement

Confirm § The package facade § Coexistence's claims match what the two goldens
jointly exercise: symbol-level distinctness (no fixed top-level names, runtime
support included), same-package disambiguation through the configured
namespace, and interleaved liveness. If the spec text promises something the
goldens do not exercise (or vice versa), flag the gap rather than assuming
either side.

## How to report

Group findings into:

1. **Missing backend** — a shipping backend absent from either side of either
   witness's build blocks (§ 1).
2. **Weakened fixtures** — either the distinct packages are no longer
   maximal-collision or the same-package trees no longer differ only by their
   configured namespaces (§ 2).
3. **Fake coexist arm** — a runner arm that doesn't host two live,
   interleaved instances (§ 3).
4. **Spec/witness drift** — the Coexistence text and the golden disagree
   (§ 4).

For each finding, cite the file (build block, runner `file:line`, or spec
section) and the anchor it violates.

**Default: report only.** If invoked with a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

---
name: audit-bundled-docs
description: Audit offline manual inventory, original-byte parity, freshness, source-package completeness, binary-only payload placement, and recorded archive/size acceptance evidence
allowed-tools: Read, Grep, Glob, Bash
---

# Bundled documentation audit

Anchor: [`docs.md` § Website and offline bundle inputs](../../topics/docs.md#website-and-offline-bundle-inputs),
also referenced by [`specs.md`](../../topics/specs.md). Read those sections and
[`specs/cli.md` § `kio manual`](../../../specs/cli.md#kio-manual) first.
This audit inspects existing checks/evidence; it does not launch release builds
or platform runs without their required authority and resource coordination.

## Inventory and contents

Compare the packer's explicit tracked-file inventory with `docs/`, `specs/`,
and `KIO-FOR-AGENTS.md`. Check regular non-Markdown assets, tracked ignored
files, exclusions of untracked/generated inputs, rejection of symlinks and
unsupported ancestors, stable ordering, and path safety. One inventory must
produce the index and finished single-file tar.gz members. Confirm original
file bytes, normalized tar/gzip metadata, compressed offset/length bounds,
content identity, and executable-version correspondence.

Trace `ls`/`tree` as index-only operations, selective bounded decoding for
`cat`, verbatim compressed-member copying for the default export, and one
conventional tar end marker for each alternative. Check streaming rather than
whole-tree buffering or extracted temporary files. Require executable tests
for all-file parity, long paths, binary assets, empty bodies, malformed paths,
wrong entry kinds, output failures, and terminal archive refusal. Inspect
causal negative evidence rather than relying only on a green check.

## Freshness and packaging

Inspect the ordinary CI freshness check and its shared packer implementation.
Verify add/edit/rename/remove rebuild evidence, including Git inventory changes;
stale generated snapshots must fail validation. Current checkout sources stay
canonical; packaged-crate inputs contain a complete snapshot, not sibling-path
includes or network fetches. Check the actual Git-free, sibling-free packaged
crate build and installed-binary run from an unrelated directory with network
access disabled. Check both normal and distribution-wrapper binary routes.
Confirm library-only, browser/Wasm, and internal Prime artifacts contain no
manual payload. Docs-only changes must not alter semantic compiler-cache keys.

## Recorded acceptance evidence

Require real independent-reader extraction and byte parity for every format,
including every entry after the first inner archive. Native Windows acceptance
uses Windows-supplied `tar.exe`, not Git/MSYS/WSL: record its version, exact
published pipe and named-file commands, Command Prompt and PowerShell >=7.4
binary-transfer results. Require actual supported macOS/Linux reader results;
source research or a Linux bsdtar run cannot stand in for native OS evidence.
Conventional formats work without `--ignore-zeros`. A default-format failure
requires a maintainer format/default decision, not an unannounced fallback.

Inspect compression-library adoption/maintenance, the complete resolved feature
graph and single default compression engine, and release-size measurements for
baseline, raw embedding, and actual compressed payload/index/codec overhead.
Require fresh-process listing, first/middle/last reading, all-format export and
export-plus-extraction measurements; do not infer an end-to-end speedup from
avoided recompression. Inspect the bounded external docs-first authoring trial
and reported fallbacks, separately from normal castle access-path checks.

## Report

Separate inventory/byte, freshness/package, command/stream, artifact-placement,
and unverified platform/measurement findings. Cite the responsible source,
ordinary CI check, and actual recorded evidence. Report missing evidence as
unverified, not as a pass. Public CLI/help quality belongs additionally to
`audit-cli-output`; offline learning quality to `audit-docs-drift`; website
rendering remains with `audit-website`.

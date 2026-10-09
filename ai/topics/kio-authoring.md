# Kio authoring in this repository

General authoring and navigation live in
[`KIO-FOR-AGENTS.md`](../../KIO-FOR-AGENTS.md), available from the installed
compiler with `kio manual cat KIO-FOR-AGENTS.md`. Start with
[`docs/README.md`](../../docs/README.md) for ordinary language and tooling
questions; specs settle precise contracts. This supplement owns only
compiler-repository obligations.

## Repository authoring boundaries

- Read [`surface-forms.md`](surface-forms.md) before introducing a surface
  form. Surface Kio becomes Lowered and then Kio'; compiler maintenance must
  preserve that phase boundary. For name resolution, imports, dispatch, or
  inference design, read [`open-world.md`](open-world.md) and state the
  open-world argument explicitly.
- A golden is a cross-implementation language, diagnostic, or runtime case.
  Follow [`TESTING.md`](../../TESTING.md) and
  [`test-data/README.md`](../../test-data/README.md) for its execution contract.
  Prefer `run.args` or `run.test-only`; `run.sh` is for nonstandard CLI or
  language workflows. Case-owned code treats generated host files as opaque.
  Durable generated-artifact assertions and independently authored public-host
  consumers belong under [`test-data/emissions/`](../../test-data/emissions/README.md);
  private compiler invariants belong in unit or mutation tests. Kio' remains
  inspectable when its phase artifact or dynamic-loading boundary is the subject.
- Reusable corpus abstractions follow
  [`test-data/poc/README.md` § General-purpose abstraction ownership](../../test-data/poc/README.md#general-purpose-abstraction-ownership).
  Do not introduce competing public owners or mistake materialized shared
  dependencies for the case's own subject.
- The public [Kio' debugging guide](../../docs/guides/debugging-with-kio-prime.md)
  owns inspection commands. Reading the backend-neutral phase artifact does not
  authorize golden-owned inspection of generated host-language files.

## Improving this guide

When authoring exposes missing knowledge, identify its proper home before
editing. Verify the claim against a spec or working example; report a compiler
defect rather than teach a workaround. Sharpen an existing pointer when the
knowledge was present but hard to find. Put ordinary language or idiom teaching
in the relevant public guide, a portable agent workflow in
`KIO-FOR-AGENTS.md`, and repository-only obligations here. Follow the owning
topic and approval gates for the destination. Surface uncertain guidance for
review, and keep every addition context-free under [`no-leak.md`](no-leak.md).

[`audit-kio-guide`](../skills/audit-kio-guide/SKILL.md) checks both authoring
files, substantive agreement with docs/specs, discoverability, and the absence
of repository-only obligations in the portable workflow.

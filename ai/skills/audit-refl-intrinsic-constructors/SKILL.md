---
name: audit-refl-intrinsic-constructors
description: Verify reflected intrinsic term constructors stay in 1-1 name correspondence with __intrinsics__ and remain separate from core term constructors
allowed-tools: Read, Grep, Bash
---

# Reflection intrinsic constructor audit

Anchors: [`specs/language.md` § Module system](../../../specs/language.md#module-system),
**Compile-time helper import**, and [`specs/formal/elaboration.md` § 7.4. Elaborator bangs](../../../specs/formal/elaboration.md#74-elaborator-bangs),
the `__comptime__` checked-term ABI. Read the applicable constructor contracts first.
This audit distinguishes core `__term_*__` constructors from reflected
intrinsic calls `__intrinsic_*__`. An intrinsic such as `__pair__` has the
reflected constructor `__intrinsic_pair__`; inventory, typing, and evaluation
must agree on that mapping.

Default to source and existing evidence inspection. Name parity alone does not
prove that a constructor evaluates or reconstructs the intended intrinsic.

## 1. Inventory and name parity

Read `PRIME_INTRINSICS` in `kio-rs/src/pass/resolve.rs`, the reflection schemes
in `kio-rs/src/pass/typecheck_core/synth.rs`, and `ComptimeBuiltin`'s public-name
mapping in `kio-rs/src/comptime.rs`. For each intrinsic, derive exactly one
`__intrinsic_<name>__` spelling by stripping its enclosing double underscores.
Compare both directions: missing and extra entries are findings. Check mapping
multiplicity and inverse consistency, not just sets of strings found anywhere
in a file. Comments and repeated references are not extra declarations.

Useful discovery:

```sh
rg -n 'PRIME_INTRINSICS|__intrinsic_' kio-rs/src/pass/resolve.rs \
  kio-rs/src/pass/typecheck_core/synth.rs kio-rs/src/comptime.rs
```

`comptime.rs` owns names and classifications, not evaluator dispatch. Do not
describe finding a name there as finding an evaluator arm. Read each scheme
against the intrinsic's argument/result contract; a matching spelling is not
type-scheme parity.

## 2. Trace evaluation and reconstruction

For each mapped constructor, follow its `ComptimeBuiltin` variant through
`kio-rs/src/normalization.rs` and
`kio-rs/src/normalization/execution/reflection.rs`: primitive application,
direct evaluator lowering/execution where applicable, checked-term creation,
and reconstruction into the corresponding ordinary intrinsic call. Mark a
shared or delegated path explicitly rather than requiring a separate local
match arm. An inventory entry, fallthrough, or unevaluated primitive is not a
successful implementation of a reducible constructor.

Record existing unit and user-elaborator evidence that reaches each distinct
path and checks its reconstructed term/type or observable behavior. A search
hit or unexecuted test is source coverage, not execution evidence. Report
missing paths and missing evidence separately. Any requested execution follows
[`local-ci.md` § Local gate coverage](../../topics/local-ci.md#local-gate-coverage);
this audit does not require a broad matrix or compiler build by default.

## 3. Public vocabulary and examples

Inspect the compile-time helper contract and the worked elaborators under
`test-data/poc/elab/`. Public imports use `__comptime__`; core term builders use
the documented `__term_*__` names, while intrinsic-call builders use
`__intrinsic_*__`. Search for old `__refl__` imports and bare `term_*` builders
as drift candidates, distinguishing unrelated local helpers and intentional
rejection tests. Classify canonical source owners rather than treating copied
dependencies as independent definitions.

Verify the documented hygienic expansion rule: callers do not acquire a
requirement to import `__intrinsics__` merely because an elaborator emits an
intrinsic call. Retain the distinction between core-language construction and
intrinsic construction; do not widen the builtin surface to make the lists
agree.

## 4. Report

For each finding, cite the contract, constructor, source owner, and whether the
defect concerns inventory, scheme, dispatch/reconstruction, public vocabulary,
or evidence. Keep inspected source, existing execution receipts, and unverified
paths distinct.

**Default: report only.** For a fix directive, follow
[`audit-fix-mode.md`](../../topics/audit-fix-mode.md). A builtin-module change
still requires its specific approval; this audit supplies no such authority.

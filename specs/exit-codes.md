# Kio exit codes

This document specifies the Unix **exit status** (the integer returned by `exit(3)` / read by the shell from `$?`) that a Kio implementation should return on completion — `0` on success, and a distinct non-zero code per error category on failure.

Kio's error **messages** are implementation-specific: different implementations will phrase the same underlying error differently, and the language does not dictate their wording. To keep tests portable across implementations, Kio implementations instead report the **category** of an error through their exit code.

The table below is a **convention**, not a hard requirement of the language. Implementations are strongly encouraged to follow it so that implementation-agnostic tests (see `test-data/goldens/`) can assert "this program should fail with a type error" without pinning a specific error message.

## The codes

| Code | Category | When it fires |
|---|---|---|
| 0  | success                  | the command completed successfully |
| 1  | internal error           | an unexpected condition the implementation didn't plan for — a bug |
| 2  | CLI usage error          | bad flags, unknown subcommand, missing required argument |
| 10 | compile error (non-specific) | a compile-time error the implementation cannot assign a more specific 1x code to |
| 11 | parse error              | unexpected token, unterminated string, invalid or missing literal suffix, malformed syntax (including forbidden `rec type`, a redundant singleton `rec`, or an ill-formed/non-minimal bare type `rec` group), identifier-spelling violation (a type name outside `_?[A-Z][a-z0-9_]*`, or a value name with an uppercase initial), a letterless name (a value name with no letter, such as `_1`; bare `_` is the wildcard binder, not a name), or a user identifier that begins with two consecutive underscores (the `__name__` reserved shape) |
| 12 | import error             | the module named by an `import` statement cannot be found, the module does not export the requested `pub` identifier, the intra-package `import` graph contains a cycle, or package source walking finds multiple package files |
| 13 | name resolution error    | unbound identifier (including a recursive singleton missing its required `rec` marker or an ordinary reference to a declaration that appears later), duplicate declaration, two distinct source identities bound to one visible name, shadowing a reserved word |
| 14 | type error               | type mismatch, wrong arity, an invalid `host type` parameter list (a higher-kinded parameter, or any parameters on a role-bearing type), a literal that three-tier resolution cannot type (no admitting `role(...)` type, or an ambiguous one), a role-bound intrinsic with no unique required `role(...)` identity, or a user-defined elaborator returning `__type_error__(message)` from the generated checked term |
| 15 | elaborator error         | **Algebraic imported elaborators** (`iso!`, `into!`, `onto!`, `align!`, `ease!`, `atom!`): no valid coercion under the form's rule subset, or `ease!`'s target requires factor duplication, or `atom!` cannot pick a single value (atomic-source rejection, multi-arm-sum target rejection, no admissible single arm in the source DNF). **Spine imported elaborators** (`reorder_sum!`, `reorder_prod!`, `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`, `flatten_sum!`, `flatten_prod!`, `one_sum!`, `one_prod!`, `fit!`): pre-condition failure (axis mismatch, spine-multiset mismatch, polymorphic source / target at `fit!`'s function-arrow walk, `flatten_*` target-required diagnostic, `flatten_*` source-shallower-than-target, `flatten_*` right-axis mismatch) or rule-search failure (`fit!`'s recursion hits a shape it cannot bridge — cross-axis without a narrow/widen path, distributivity-only rewrite). **`derive!`**: resolution failure (`derive_no_rule_applies` — no candidate rule derives the goal; `derive_ambiguous` — two or more derivations; `derive_kind_mismatch` — a candidate binder's kind disagrees with the goal binding) or rule-shape rejection (`derive_rule_not_decreasing` — a precondition is not structurally smaller than the result; `derive_rule_fresh_binder` — a precondition mentions a binder absent from the result). **User-defined elaborators**: returning `__elab_error__(message)` from the generated checked term. Returning `__type_error__(message)` reports under code 14 instead. |
| 16 | totality error           | strict-positivity violation in a recursive `newtype` component, an alias-only cycle (including an alias-only subcycle inside a nominal group), or a `__structural_recur__` evaluation that gets stuck because its fuel is not structurally decreasing (a loop-via-arrow that does not decrease). `match!` non-exhaustiveness, an unreachable `match!` clause, and a `match!` clause that covers no scrutinee branch are **not** here: `match!` is a user-defined elaborator, so those surface as elaborator errors (code 15) via `__elab_error__` |
| 20 | bridge error (non-specific)  | an error in the package's host contract surface the implementation cannot assign a more specific 2x code to — e.g. a `bridge` glob matching no module (dead glob), a host-bearing module reachable from a bridged module but selected by no glob (module completeness), or a type a bridged signature reaches that is not itself exposed (type closure) |
| 30 | dependency / resolution error (non-specific) | a cross-package dependency declared by a `<local>.dep.kio` file (see [`package.md` § Dependency files](package.md#dependency-files)) cannot be resolved into the consumer package, and the implementation cannot assign a more specific 3x code to it — e.g. the `source { path ... }` does not resolve to an existing `*.pkg.kio` package file, the dependency's local name collides with the first path segment of a local module (local-name / local-root collision), or the resolved dependency does not itself collect / parse |
| 40 | build error (non-specific)   | a build-time error the implementation cannot assign a more specific 4x code to — e.g. a missing `<name>.pkg.kio` package marker, a package file with no `build { ... }` block, a build block with duplicate target ids; a target whose backend is unknown or an unknown explicit target id on the CLI; invalid keys for a backend; backend transpilation failure |
| 50 | test failure (non-specific)  | a `kio test` run found at least one `equiv` block whose `term`s did not all reduce to the same residual normal form, or otherwise failed to verify cleanly. Finer-grained `51`–`59` are reserved for later (e.g. property-test counterexample, evaluator runtime error) |
| 60 | fmt check                    | `kio fmt --check` found at least one file whose canonical form differs from disk. Distinct from `gofmt -l` / `cargo fmt --check`, both of which use `1` (Kio reserves `1` for the internal-error category — see § Tiers). The dirty path list is printed on stdout per `cli.md` § `kio fmt`. |
| 70 | doc error (non-specific)     | `kio doc` (see [`kiodoc.md`](kiodoc.md)) found a contract violation in a markdown file — undeclared harness, multi-`@NAME` on one fence, forward harness reference, duplicate harness declaration, repeated attribute, unknown attribute, missing output fence, orphan output fence, intervening fence between a snippet and its declared output fence, attribute-less ` ```kio ` fence — or a snippet whose `kio check` invocation exited with a code other than its declared `check_exit_code` (default `0`). Finer-grained `71`–`79` are reserved for later. |
| 80 | sig incompatibility          | `kio sig status` (see [`versioning.md` § The `kio sig` command](versioning.md#the-kio-sig-command)) found that the source breaks the last **sealed** contract and the break is unrecorded. Acknowledge it with `kio sig stage --force` or reconcile the source. |
| 81 | sig stale-but-compatible     | `kio sig status` found an unrecorded **compatible** contract-surface delta. Run `kio sig stage` to record it. |
| 82 | sig unsealed-break-pending   | `kio sig status` found a recorded break that has not yet been sealed by `kio sig commit`. Run `kio sig commit` to seal it. |

**Tiers.** Codes are grouped by concern, not by pipeline position:

- `1x` — compile errors (single-package: parse, import, name-res, type, elaborator, totality).
- `2x` — bridge errors (issues with the current package's `<name>.pkg.kio`).
- `3x` — dependency / resolution errors (resolving a `<local>.dep.kio` cross-package dependency into the consumer package). Code `30` is the reserved code for this tier; `31`–`39` are reserved for later finer-grained distinctions.
- `4x` — build errors (target resolution and backend failures in `kio build`).
- `5x` — test errors (`kio test` runtime failures: an `equiv` block didn't verify, etc.).
- `6x` — fmt-check tier (`kio fmt --check` found differences). Code `60` is defined; `61`–`69` are reserved.
- `7x` — doc-check tier (`kio doc` found a markdown / snippet-validation failure). Code `70` is defined; `71`–`79` are reserved.
- `8x` — sig-status tier (`kio sig status` reports the package's contract-surface compatibility state). Codes `80` / `81` / `82` are defined; `83`–`89` are reserved. The three are assigned by the precedence order in [`versioning.md` § The `kio sig` command](versioning.md#the-kio-sig-command), not by independent predicates.

**Multiple errors.** The exit code identifies the diagnostic that caused the
command to fail. When one invocation contains several independent errors, Kio
requires deterministic selection for the same inputs and environment but does
not specify which diagnostic, source location, category, or exit code is
selected. An
implementation may stop at the first error it encounters or select another
useful diagnostic. Category numbers identify errors; they are not priorities
and do not impose a global phase order.

A test that asserts an exit code therefore isolates the error whose category it
intends to test. Command-specific contracts may define precedence among their
own overlapping states; `kio sig status` does so for `80` / `81` / `82`.

**Reserved ranges.** Codes 0–2 follow POSIX conventions. Kio-specific categories start at 10 and may grow as new language features introduce new categories; unused numbers within and between tiers are reserved for future insertions — don't reuse them for something unrelated. Avoid codes ≥ 126, which collide with shell and signal conventions.

## Runtime exit codes

The table above categorizes a Kio **tool** run — `kio build`, `kio check`, `kio test`, and the rest of the CLI. It does not govern a Kio **program** that a host has already built and run: once the artifact executes, it exits with the status its own `main` (or the host driving it) chooses, and that status is not a compiler category. A program that completes normally exits `0`; one that deliberately aborts exits with whatever code it hands its host `exit` — Kio imposes no panic-status convention of its own.

The goldens collect these built-and-run cases under the `90_runtime_exit` tier (see [`test-data/README.md`](../test-data/README.md) § Golden test case layout). Each such case asserts the program's own runtime code through its own `expected.exit`, so the `90` prefix is a tier label rather than a single asserted category: a case there may build cleanly and then exit `1`, `90`, or any other program-chosen status.

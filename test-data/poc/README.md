# POC corpus (`test-data/poc/`)

Adopter-grade Kio modules that double as `kio check` + `kio test` + per-backend `main`-run coverage. Each `test-data/poc/<topic>/` carries a reusable root package under `workdir/`; library POCs also carry a nested `workdir/demo/` package that imports the root package the way an adopter would. The root library sources are shaped so a user can copy them into their own Kio package and have a working library — the source headers and `///` docs say so explicitly.

The contract mismatch between *test fixture* and *user-facing reference code* is why this corpus is separate from `test-data/goldens/`: goldens optimize for stability and minimality, while adopter libraries need to evolve with feedback. Putting the modules here makes that honest. Goldens still pin individual language-feature behaviors; POCs pin the ergonomics of a full data-structure library.

## Case layout

The corpus root keeps this `README.md` as the shared contract for POC authors and audits. Each `test-data/poc/<topic>/` carries the same standard layout as a golden case:

- `workdir/` — the reusable Kio package, with `<topic>.pkg.kio` (carrying its `build { ... }` block), the library modules, and any materialized `*.dep.kio` dependencies. Library package names use the topic name (`optics`, `hkt`, `list`, …), not abbreviations.
- `workdir/demo/` — when present, a separate runnable package with `<topic>_demo.pkg.kio`, a `library.dep.kio` import of the root package, adapter modules such as `list_host.kio`, and runner-facing `testapi` modules. Worked examples whose subject is the runnable mechanism itself may keep their entry package directly under `workdir/`.
- `run.sh` — chains root-package `kio check`, root-package `kio test`, then demo-package or worked-example build + runner execution. All steps must exit `0`.
- `expected.stdout`, `expected.exit`, and exactly one stderr policy file
  — checked against the actual run output. `expected.exit` must contain
  `0`, with or without a final newline; non-zero expected exits belong
  in `test-data/goldens/`. `expected.stdout` snapshots `main`'s output
  (the `kio check` / `kio test` output is redirected away in `run.sh`).
  Successful POCs normally use `expected.stderr.ignore`; see
  [`test-data/README.md`](../README.md) for the full stderr policy.

## Case inventory

The corpus intentionally keeps both `option` and `result`.

- `option` models a value that may be absent. Its laws and demonstrations
  cover the zero-or-one container shape: `some` / `none`, `map`, `bind`,
  `or_else`, conversion to and from `A | .`, and collection helpers such as
  `cat_options`.
- `result` models a computation that either succeeds or carries a typed error.
  Its surface is right-biased and error-aware: `ok` / `err`, `map_err`,
  `bimap`, `swap`, recovery helpers, and applicative / monadic operators over
  `Result(T, E)`.

Those two APIs overlap at `map` / `bind`, but they exercise different adopter
contracts: absence versus recoverable typed failure. Keeping both makes that
distinction concrete instead of overloading one POC with two meanings.

## General-purpose abstraction ownership

Each general-purpose abstraction and its literal syntax has one POC owner.
The [`list`](list/) package owns generic `List` and its `[...]` literal;
the dedicated [`option`](option/) package owns the named `Option` abstraction.
Another POC must not introduce a duplicate public general-purpose collection
or optional-value abstraction, even under a different name.

Use ordinary products and sums or private, domain-specific implementation
state when no shared abstraction is needed. When a public API genuinely needs
another package's nominal collection identity, declare a dependency on that
owner instead of cloning the type or literal. If that dependency would be
disproportionate, expose a fold, visitor, or callback seam and let the caller
choose its collection.

Routine absence outside the dedicated Option abstraction uses `A | .`, with
`()` injected into the absent arm, rather than another local named Option.
Classify a representation by its domain role and actual API: recursive shape
or public visibility alone does not make it a general-purpose duplicate.
Preserve a domain-specific raw-representation contract when it is not a duplicate
general-purpose abstraction.

Committed materialized dependencies are derivatives of their declared source
owner, not additional authored abstractions. Review ownership at that source;
the dependency declaration and canonical-materialization checks still apply.

An ownership-policy or audit-strengthening change lands only with a
corpus-wide review that resolves or explicitly reclassifies every existing
violation. A policy-only change must not knowingly leave the corpus in
violation of its new rule.

## Host-facing demos

Every POC still has a `main` so the test runner can execute a concrete tour and
snapshot `expected.stdout`, but reusable libraries keep that runner-facing code
out of the root package. The root package declares the host capabilities its
public API genuinely needs (`host type`, `host fn loop`, arithmetic, string
conversion, key comparison, and so on). The nested demo package imports the
root package through `library.dep.kio` and uses `rehost` to bind those host
requirements to small local adapter modules such as `list_host.kio` or
`dict_host.kio`.

`testapi/` is reserved for the test-runner protocol: `demo/testapi/main.kio`
is the demo entry, and `demo/testapi/**` names the canonical runner-provided
host functions. Do not put reusable library host requirements under
`testapi/`, and do not invent a bundled capability parameter when direct host
declarations and `rehost` express the same boundary.

## Module documentation

Every root-package POC library `*.kio` file is copyable library source. It must have module-level `///` docs immediately above the `module` directive, and every exported top-level declaration must have attached `///` docs: `pub fn`, `pub type`, `pub literal`, `pub alias`, `pub newtype`, `pub labels`, `pub op`, `pub fold`, and `pub elab`.

Demo modules, host adapters, materialized dependency roots, and `testapi` modules are not part of the root library documentation requirement. A demo may still document its public entry points, but the required adopter-grade `///` surface is the root package's own library surface.

Private helpers do not need `///`. If a declaration is not worth documenting for copyable library users, do not make it `pub` unless another module genuinely needs it across the module boundary. Public constructors and projectors declared inside a documented `newtype` are covered by the `newtype` docs unless they carry extra semantics that need their own explanation.

## Running

The orchestrator at `ci/checks/orchestrators/poc-tests.sh` walks `test-data/poc/*/` and runs each case against every applicable emitter. Flags match `golden-tests.sh`: `--impls=FULL_IMPL_MATRIX`, `--impls=SAMPLE_IMPL`, `--impls=<name>[,<name>...]`, `--jobs=<N>`, and positional EREs after `--` filter cases.

```sh
sh ci/checks/orchestrators/poc-tests.sh
sh ci/checks/orchestrators/poc-tests.sh --impls=kio@js
sh ci/checks/orchestrators/poc-tests.sh --impls=kio@js -- elab
sh ci/checks/orchestrators/poc-tests.sh -- optics
```

## Editing a POC other cases depend on

A POC package is reusable, so other cases declare it as a dependency and
commit the re-rooted module tree `kio dep fetch` materializes from it —
comments included. Changing a single character of a POC's library source
therefore stales every consumer's committed tree, and `dep-canonical`
fails those cases until the trees are regenerated. Regenerate and commit
them in the same change:

```sh
(cd kio-rs && sh ../ci/cargo.sh build --bin kio)
sh ci/checks/repo-lint/dep-materialization.sh --write
```

`git grep -l 'poc/<topic>' -- '*.dep.kio'` names the consumers.

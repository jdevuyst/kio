# Generation-baseline implementation evidence

The contract lives in [package configuration](../specs/package.md#per-target-keys)
and the [shared backend specification](../specs/backends/README.md#generation-baselines).
This record describes the initial fixed-table implementation and the scope of
its focused validation. Compiler-only results and native compatibility results
are recorded separately below.

## Applicability and ownership

The semantic law applies to every host backend: a target's generation baseline
is resolved from its configuration and a compiler-owned finite table before
emission or cache reuse. Omission and Unit resolve to the table's latest entry
and canonical formatting materializes that entry. Explicit `latest` remains
floating; a concrete selector remains concrete.

| Backend | Concrete selector | Shared resolver, formatter, and build goldens | Additional cache occurrence |
| --- | --- | --- | --- |
| JavaScript | `es2020` | pass | JS module-emission profile |
| TypeScript | `v5_0` | pass | shared JS runtime maps to `es2020` |
| Python | `v3_10` | pass | resolved artifact profile |
| Rust | `v1_85` | pass | resolved artifact profile |
| Go | `v1_26` | pass | resolved artifact profile |
| Java | `v21` | pass | resolved artifact profile |
| Swift | `v6_0` | pass | resolved artifact profile |
| Haskell | `ghc9_10` | pass | resolved artifact profile |
| Kio' output | none | baseline configuration is rejected | no host-generation baseline |

The fixed table in `build_target` also owns field choices used by the parser,
formatter, and editor completion. It does not inspect module declarations,
imports, installed tools, or network state. Adding a declaration therefore
cannot change baseline selection or the meaning of downstream code: the
open-world argument follows from the resolver's inputs, not a corpus sample.

The artifact cache includes the resolved baseline. A focused JS cache test
checks both sharing of the same JS/TS runtime profile and separation of distinct
profiles; its synthetic alternate profile is a private cache-test value, not an
additional accepted source selector.

## Language and tooling boundary

Recognized closed configuration choices may contain version words such as
`v5_0`; ordinary value identifiers retain their existing naming grammar.
Negative unit assertions cover unrelated configuration fields and ordinary
declarations, so this allowance does not become a general identifier extension.
The first parse/reparse run rejected recognized version words under the ordinary
identifier rule; the field-catalog-specific correction made those tests pass.

Package parsing, CLI formatting, LSP completion/formatting, Kiodoc package
formatting, initialization, and generator package templates are affected. The focused evidence
is six baseline tests, 241 formatter tests, 57 completion units, 33 live LSP
completion tests, four live LSP formatting tests, seven Kiodoc formatting tests,
three target-catalog tests, and ten generator emission tests, all passing.
The reduced `prime,cli,parallel` feature set passes `cargo check` for
`kio-prime`; the CLI fixture-preparation helper does not depend on surface-only
features.

The highlighting fixture
[`generation_baselines_package`](../test-data/highlight-corpus/generation_baselines_package/)
covers every concrete selector plus grouped, Unit, and floating forms. Native
tree-sitter parsing and focused compiler/tree-sitter/TextMate agreement pass.
Existing token grammars already admit these configuration words, so no editor
grammar or extension lifecycle change is needed. LSP definitions, references,
rename, and declaration code actions are unchanged because selectors are closed
configuration values, never declaration references. REPL expression parsing and
completion likewise have no package-target context. Regular modules, dependency,
lock, and signature file syntax is unchanged. Typing, lowering, normalization,
phase validation, and signature extraction receive no new term form or authority.

## Focused configuration goldens

`baseline_selection`, `baseline_string`, `baseline_duplicate`, and
`baseline_unknown` pass all 64 explicitly selected backend/compiler rows:
eight host targets through both full and Prime CLI entry points. The cases
exercise omission, grouped Unit, grouped `latest`, grouped concrete selection,
format/reparse/build, and the three isolated error categories. Both case-binary
canonical-format and Prime-marker checks pass. These configuration-only cases
do not claim emitted-runtime execution.

The existing formatter subjects `fmt_configuration_defaults`,
`fmt_build_optional_cache_omission`, `fmt_doc_comments`,
`fmt_named_entry_order`, and `fmt_package_file` retain their input subjects and
pass all 15 applicable full-CLI target rows plus their five case-binary checks.
The exact expected-output changes add only materialized baseline lines.

The initialization template also uses the shared catalog to emit a concrete
baseline. Before that correction, both its existing canonical-output unit and
`kio_init_package_checks_clean` golden failed because `kio fmt --check` required
the missing baseline. This is an integration regression of baseline
materialization, not a changed ordinary-identifier or package-checking rule.

### Validation receipt

The following results summarize command output; raw test logs are not included
in this record. Cargo commands ran through `ci/cargo.sh` with the owning
manifest. Focused unit groups also ran directly from the freshly built test
executables, without recompiling between filters.

| Command or executable filter | Result |
| --- | --- |
| `cargo test --locked --lib baseline` | 6 passed |
| library filter `pretty::tests` | 241 passed |
| library filter `lsp::completion` | 57 passed |
| library filter `kiodoc::fmt::tests` | 7 passed |
| library filter `build_target::tests` | 3 passed |
| `lsp_smoke` filter `completion` | 33 passed |
| `lsp_smoke` filter `formatting` | 4 passed |
| library filter `cmd::init::tests` | 4 passed after the scaffold correction |
| `cargo check --locked --manifest-path kio-rs/Cargo.toml --no-default-features --features prime,cli,parallel --bin kio-prime` | passed |
| `cargo test --locked --manifest-path ci/infra/kio-gen-rs/Cargo.toml --lib emit::tests -- --quiet` | 10 passed |
| `cargo fmt --manifest-path kio-rs/Cargo.toml --check` | passed |
| `cargo fmt --manifest-path ci/infra/kio-gen-rs/Cargo.toml --check` | passed |
| `cargo clippy --locked --manifest-path kio-rs/Cargo.toml --all-features --all-targets -- -D warnings` | passed |
| `cargo clippy --locked --manifest-path ci/infra/kio-gen-rs/Cargo.toml --all-targets -- -D warnings` | passed |

The formatting and strict Clippy commands used the verified development Rust
environment through `ci/impl-toolchain.sh toolchain-exec rust -- sh ci/cargo.sh`:
Rust 1.99.0, rustfmt 1.10.0, and Clippy 0.1.99. These are compiler-source hygiene
checks, not evidence for generated-output compatibility at a host endpoint.

For the configuration goldens, the exact harness selection was
`--impls=FULL_IMPL_MATRIX --sample-cases=all` with this anchored filter:

```text
^((00_success/baseline_selection)|(11_parse_error/baseline_string)|(40_build_error/baseline_(duplicate|unknown)))$
```

Each of `js ts python rust go java swift haskell` had an explicit
`--impl-def=name=<compiler>@<backend>,kio=<frozen-binary>,runner=/usr/bin/false,target=<backend>`
for both compiler entry points. `fmt-canonical.sh` and `prime-marker.sh` were
supplied as absolute `--check` paths, with the independent Prime checker
selected through `KIO_PRIME_CHECK_BIN`. The false runner makes any unintended
host-runner invocation fail. Result: 64 case rows and eight case-binary checks
passed, exit zero.

The formatter-oracle run used the same harness settings and checks with only
the full compiler and this anchored filter:

```text
^00_success/(fmt_configuration_defaults|fmt_build_optional_cache_omission|fmt_doc_comments|fmt_named_entry_order|fmt_package_file)$
```

Result: 15 applicable target rows and five case-binary checks passed, exit zero.
Neither command installed or executed a native host toolchain.

After the initialization correction, the same full-compiler harness settings
with `^00_success/kio_init_package_checks_clean$` passed eight target rows and
one case-binary invariant group, exit zero. Its original `check` and
`fmt --check` assertions were unchanged. The refreshed executables came from
`cargo test --locked --manifest-path kio-rs/Cargo.toml --lib --test lsp_smoke --no-run`
through the repository wrapper; the earlier baseline configuration results were
not rerun for this isolated source-template change.

## Canonical migration classification

A single tracked-package inventory classified 1,616 manifests as baseline-only
formatting changes, 175 as unchanged, 54 as deliberately retained source
subjects, and eight as retained nonparsing inputs. The landing example was
separately excluded from that inventory's writes. For each migrated manifest,
removing the inserted concrete selector tokens recovered the complete original
non-comment token stream, and the ordered comment texts were identical. No
declaration, target, namespace, dependency, or test assertion was removed.

Ten guide/tutorial/host pages were formatted as one batch. Removing the inserted
baseline lines recovered their previous bytes exactly. Their Kiodoc checks and
the documentation formatting check pass. The five formatter oracles were
updated through their existing scripts or formatter input, with the same
reverse-removal check.

This migration preserves the effective baseline under the initial table. It
intentionally changes later selection from implicit floating to explicit pinned
selection, as required by the formatter contract; it is not a claim that every
future compiler produces identical output. Tests whose subject is omitted,
Unit, floating, invalid, or noncanonical input retain that source, and the
parser-owned fixture helper preserves all nonselected target selectors.

Native minimum/current endpoints, retained prior-release output, and unchanged
host-source compatibility are separate obligations described in
[`TESTING.md`](../TESTING.md). In particular, ordinary JS/TS runtime evidence uses
the locked QuickJS runner; a selected TypeScript checker or Node installation
does not establish execution under a different runtime.

## Harness selection and source ownership

The shared [environment map](../ci/infra/baseline-environments.tsv) contains
environment IDs, not a second table of exact tool versions. The provisioning
owner resolves those IDs and verifies the tools before harness workers start.
The deterministic baseline selftest passes, including backend-first sampling,
one sampled configuration per case, endpoint deduplication, seed replay,
current-only full-matrix expansion, current-only contribution execution,
unavailable-environment failure, and installer preparation from that same map.
Existing dispatch, fixture-ownership, relative-path, compiler-proxy, and
top-level forwarding selftests also pass.

Ordinary baseline-parametric cases use a private copy after source dependency
fetch and case-binary checks. The selected manifest is transformed through the
compiler parser; implementation checks and execution share that copy. Fixed
selector subjects and custom scripts preserve their sources. Environment
fragments are prepared once per selected language/environment, and workers do
not install tools or retry with a different endpoint.

Relocation materializes file-symlink payloads in the private copy and preserves
non-file link resolution without traversing linked directories. The original
copy-only implementation lost a relative link to a sibling case. Its regression
selftest fails before the repair and passes afterward, covering source-write
isolation, whitespace and newline paths, directory links, cycles, dangling-link
collisions, and the exact-shape admission guard.

An actual `castles/order_book_matcher` representative exercised its relative
dependency on the sibling `poc/elab` package. Source dependency preparation
preceded copying; the detached case then built without refetching its relocated
dependency path. The complete source `.kio` inventory was byte-identical before
and after. This was a compile-only check, not runtime evidence. The representative
case contained 552,218 regular-file bytes (656 KiB allocated); one warm `cp -RP`
sample reported 0.00 seconds at the timer's resolution. That bounded observation
is not a general copy-cost or performance guarantee.

## Published output on current native endpoints

The checked-in [0.2.2 fixture](../ci/infra/backend-compatibility/retained/0.2.2/PROVENANCE.md)
contains output from the published compiler, not output regenerated by the
baseline-aware compiler. Its exact source, release artifact identity, generated
file inventory, and hashes are retained. The consumer verifies inventory and
hashes before and after testing, copies the output before native compilation,
and never invokes a Kio compiler. All eight current-tested consumers passed on
Linux x86-64:

| Backend | Observed selected consumer | Evidence |
| --- | --- | --- |
| JavaScript | Node 24.21.0 and Cargo-locked QuickJS | independent fixed public host and existing runner both return the three expected values |
| TypeScript | Node 24.21.0, TypeScript 6.0.3, and Cargo-locked QuickJS | fixed public host, strict retained-sidecar check, and existing runner pass independently |
| Python | Python 3.12.15 and Pyright 1.1.411 | retained-sidecar check and existing runner pass |
| Rust | Rust 1.99.0 | existing runner freshly compiles and executes retained output |
| Go | Go 1.27.2 | existing runner freshly compiles and executes retained output |
| Java | OpenJDK 21.0.12.1+1 | existing runner freshly compiles and executes retained output |
| Swift | Swift 6.4 | existing runner freshly compiles and executes retained output |
| Haskell | GHC 9.10.3 | existing runner freshly compiles and executes retained output |

The protocol is `export-poly-roundtrip`: the host calls the public polymorphic
echo twice with distinct types and the public left-selection function, expecting
`poly-string`, `42`, and `left`. The tiny retained Node host supplies module
context outside the frozen generated tree; it neither patches generated bytes
nor substitutes Node for the ordinary QuickJS runner. Native compilation uses
fresh consumer scratch space rather than an already-built runner artifact cache.

The command shape is:

```sh
sh ci/infra/check-retained-output.sh LANGUAGE BASELINE EXISTING_RUNNER
```

Each row uses the concrete selector in the applicability table above and an
already-built immutable `kio-test-runner-LANGUAGE`. The deterministic selftest
also proves that a changed artifact hash or unrecorded artifact file rejects the
fixture before runner execution. These results establish the retained-output /
current-tool direction for this bounded public-interface subject, not every
historical output or every native platform.

## Current generation and unchanged fixed hosts

Each existing host-interface witness in the
[bounded witness map](../ci/infra/baseline-witnesses.tsv) passed with the current
compiler and its current-tested native endpoint. These are distinct from the
published-output consumers above: the compiler emits fresh output, then the
independent public calls and assertions consume it. Narrow driver-floor
corrections are recorded below rather than being attributed to compiler/API
changes.

| Backend | Existing witness | Current-tested result |
| --- | --- | --- |
| JavaScript | `opaque_structural_substitution` | Node 24.21.0: pass |
| TypeScript | `rich_typed_host` | TypeScript 6.0.3 and Node 24.21.0: pass |
| Python | `opaque_structural_substitution` | Python 3.12.15 and Pyright 1.1.411: pass |
| Rust | `host_trait_surface` | Rust 1.99.0: pass |
| Go | `source_grouped_boundaries` | Go 1.27.2: pass |
| Java | `retained_nominal_holders` | OpenJDK 21.0.12.1+1: pass |
| Swift | `exact_host_bindings` | Swift 6.4: pass |
| Haskell | `generic_public_abi` | GHC 9.10.3: pass |

The direct harness runs used explicit `kio@LANGUAGE` definitions with the frozen
compiler, `runner=/usr/bin/false`, and `target=LANGUAGE`,
`--baseline-environments --toolchain-environment=development`, and only the
anchored witness names above. The false runner is a tripwire: these custom host
scripts use their actual selected native tools rather than an ordinary runner.
No repository Cargo build, tool installation, or full corpus execution was part
of these witness commands.

### TypeScript checker distinction

The exact ordinary `00_success/ffi_export_poly_roundtrip` case passed with the
TypeScript 5.0.4 minimum checker, the existing strict per-case check, and the
Cargo-locked QuickJS runner. The selected checker ran on maintained Node 24;
this is not a Node 14 runtime result. The harness selected only `kio@ts` with an
explicit `target=ts`, the anchored case filter, `--baseline-environments`, and
`--toolchain-environment=typescript5_0`.

The existing `ts/rich_typed_host` host-interface case also passed with the
minimum TypeScript 5.0.4 checker and Node 24.21.0, using the same direct command
shape with `--toolchain-environment=typescript5_0`. Its independently authored
host compiles with `--target ES2022`; that driver requirement is distinct from
the generated TypeScript baseline's ES2020 runtime requirement. The custom
script preserves the committed manifest and does not claim an ordinary
QuickJS-runner execution.

### Minimum endpoint and driver results

The same bounded witnesses pass on Node 14.21.3, TypeScript 5.0.4 (with maintained
Node 24), Python 3.10.22, Rust 1.85.1, Go 1.26.9, and Swift 6.0.3. Java and
Haskell map their minimum and current-tested endpoints to the same environment,
so their passing rows above are shared rather than executed twice. This is
eight current endpoint rows plus six distinct minimum endpoint rows, not a
historical-tool Cartesian product.

The first Node 14 attempt failed because the driver had not supplied ES-module
context for imported `.js` files; newer Node's automatic syntax detection had
masked that prerequisite. The host now supplies standard `type: module`
package metadata at the scratch root, outside generated output. The public
consumer source and generated bytes are unchanged. Both Node 14 and Node 24
then passed without a newer-runtime-only flag.

The first Python 3.10 attempt failed in the driver's generic `TypedDict`
declarations before package execution. Those declarations are annotation-only;
with postponed annotations already enabled, they now live under
`TYPE_CHECKING`. The public calls, runtime assertions, strict positive Pyright
check, and invariant-index negative check are unchanged. Both Python 3.10 and
3.12 then passed. These observed failures and repaired executions distinguish
host-scaffolding floors from the generated program's floor.

### Generated helper floor and peer applicability

Every emitted helper must be valid at its declared floor, including helpers a
particular test program does not call. The initial Swift 6.0 run rejected the
product and sum witness-fold helpers: passing an array-subscript existential
directly to a generic factory did not open that existential. The correction
binds the array element to a local before each factory call. It changes neither
the folding algorithm nor the public host interface. The original
`exact_host_bindings` witness failed on the unfixed compiler, then passed with
the corrected compiler on both Swift 6.0.3 and 6.4, with its independent host
source unchanged.

The adjacent Swift `opaque_structural_substitution` and
`application_carrier_visibility` witnesses also pass on both Swift 6.0.3 and
6.4. The latter initially rejected the correct wrong-host assignment but its
oracle recognized only the newer compiler's detailed host-parameter mismatch.
Its older ambiguity diagnostic is now accepted only for the exact
`host_rebrand.swift` client, line, and wrong-host source assignment. The existing
positive driver first compiles the same value with its correct host identity;
the other three negative controls and the newer detailed diagnostic remain
unchanged. Both endpoint replays pass after this oracle correction.

The law is universal; implicit existential opening is the Swift-specific
realization exposed here. A bounded mechanism review of the other seven
backends found no analogous floor-dependent construct. Their actual
minimum/current witnesses above support conformance for these subjects, not
exhaustive proof over every generated shape.

The dedicated runtime regressions `exec_intrinsic_structural_witnesses` and
`exec_intrinsic_discard_sum_payload` pass the anchored two-case selection on all
eight host backends through the full compiler: 16 runtime rows and two
case-binary invariant groups. The structural-witness case was observed failing
on Swift 6.0 before its repair and passing afterward. Its first cross-backend
run also exposed a Rust discard-pattern defect: an unused payload pattern `_`
was emitted into the expression `&_`. Removing that redundant dummy borrow
preserves the payload discard and generated arm body. The separate
discard-payload case was observed failing before the Rust repair, then passing
on Rust 1.85.1; the structural-witness case also passes on Swift 6.0.3. These
pinned patch-level receipts do not establish execution on the earliest 1.85.0
or 6.0.0 compiler patches.

Both regression subjects also pass through the direct Prime compiler with the
JavaScript runner and through dynamic Prime loading: two runtime rows and two
invariant groups on each path. These four rows establish the corresponding
phase-path behavior, not native execution on every backend through Prime.

These checks establish the mapped endpoints for bounded subjects on Linux
x86-64, not every generated shape or native platform. In particular, the generic
JS/TS corpus does not run on selected Node merely because Node is installed.
Minimum Node witnesses are bounded trusted host-interface checks; Node 14 has
no official macOS-arm64 binary, so native evidence there remains absent rather
than being inferred from emulation or a newer runtime.

## Retained fixture hygiene

The frozen Rust manifest is excluded by exact path from the repository license
mirror inventory because it is generated compatibility data, not a subproject.
A staged-inventory simulation failed before that exclusion, passed afterward,
and still rejected an unrelated unregistered manifest. The historical source's
formatting exemption is likewise restricted to the retained fixture tree;
hash verification continues to protect its bytes.

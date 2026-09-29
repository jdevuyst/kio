---
name: audit-test-strategy
description: Verify spec→test coverage, golden/emission hygiene, test placement per TESTING.md, generator surface coverage, kio-prime gate, and the redundancy promise that each test layer catches what others miss
allowed-tools: Read, Grep, Glob, Bash
---

# Test-strategy audit

The test corpus in this repo is the spec, exercised against any implementation (see `TESTING.md` and [`ai/topics/repo-layout.md`](../../topics/repo-layout.md)). The interesting question is therefore **spec→test coverage**, not line-coverage of `kio-rs/`. This skill checks the corpus is structurally aligned with what the project commits to support.

Read `TESTING.md`, `test-data/README.md`, and `test-data/emissions/README.md` before starting.

## 1. Spec→test coverage

For each behavior named in `specs/`:

- `specs/language.md`, `specs/prime.md`, and `specs/formal/elaboration.md` — every surface form (operators, tuple literals, generic imported block calls including `if!`/`else`, `scope!`, `do!`, and `match!`, the reference coercion-elaborator palettes (algebraic `iso!` / `into!` / `onto!` / `align!` / `ease!` / `atom!`, spine `fit!`, `reorder_sum!`, `reorder_prod!`, `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`, `flatten_sum!`, `flatten_prod!`, `one_sum!`, `one_prod!`), `derive!`, UFCS, `fn#`, `alias`, label sugar, `op`, `equiv`) should have a golden under `test-data/goldens/` exercising parse, typecheck, and (where applicable) build. Each elaborator should have at least one *subject-focused* golden whose source's primary purpose is that elaborator's defining rule, plus a boundary-rejection golden in the matching error tier. The coercion palettes and library-defined control forms are user-defined libraries; their per-form rule semantics are exercised and documented by the `test-data/poc/elab/` package and its case study `docs/poc/elab.md`, not by `specs/`.
- `specs/grammar.md` — each production layer (Kio' / surface / package files) should have golden coverage in `test-data/goldens/`.
- `specs/package.md` — each file kind and required field should have a golden that fails when the field is missing/wrong.
- `specs/exit-codes.md` — each documented exit code should have at least one golden asserting it. Per-case checks live in `ci/checks/per-case/`.
- `specs/cli.md` — each subcommand and flag should have an exercising golden.
- `specs/backends/*.md` — each emitter should have cross-implementation build / loading / FFI runtime goldens for its documented semantics, including polymorphic host function signatures, higher-rank type parameters (`forall`-quantified arguments), existential types, and brand-based HKT. Those goldens use a fixed ordinary runner protocol and pass generated directories opaquely; the runner's ordinary host compile/load/run is the default public-interface oracle. Add backend-specific `HOST_INTERFACE` emission evidence only for a documented public-host fact that the runner cannot naturally and independently demonstrate. A mechanically-wide host may be generated only from fixed checked-in parameters and never by reading emitted output. A durable output path, manifest, language-mode, named public-file, or measured optimization fact may instead have an `ARTIFACT_SHAPE` emission. An emission that merely duplicates a runner-proven contract is a finding. Emissions supplement the runtime goldens and never satisfy their coverage cells. A genuine per-backend impossibility still needs matching concrete-caveat banners in the backend spec and host guide plus an emitter comment at the exact degradation site citing the detailed spec section.
- `specs/style.md` — formatter goldens should exercise each canonicalization rule.

Compile a list of un-covered behaviors.

### Expression-grammar compositionality

Inspect and run `kio-rs/src/pass/parser/compositionality_tests.rs`. Its
declarative grammar-keyed matrices must stay aligned with the bounded
`specs/grammar.md` inventory and cover eager/forced-lazy parsing, exact AST
topology through pretty/reparse and formatter idempotence, raw versus typed
`CallArg` ambiguity, precedence and scope boundaries, and open-world changes
that add only unselected declarations. Treat an inventory or matrix failure as
coverage evidence or a contract question; it does not authorize changing
accepted syntax or parser behavior to make the audit green.

## 2. Goldens hygiene

Deprecated-history behavior needs paired host-source evidence: an unchanged
old host still compiles where the backend claims source stability, and a new
host implementing/selecting live items only also compiles. Assertions cover
the complete emitted history-only transitive closure's deprecation markers, a
live-plus-history declaration that remains nondeprecated, and absence from
loader matching and runtime/package dispatch. A separate witness constructs
incompatible declaration epochs at one exact identity and proves the planner
omits every affected retained root and dependency; a paired live-dominance
witness proves current declaration meaning is undisturbed. A runner that
silently supplies retained items cannot substitute for the live-only host
control.

ai/topics/repo-layout.md is explicit: "Goldens are language-level fixtures, not implementation tests — case files (`*.kio` source, `run.sh`, `expected.*`) must not name implementation internals." For each case under `test-data/goldens/`:

- Grep `*.kio`, `run.sh`, `expected.*` for kio-rs function names, module paths, struct/type identifiers.
- Comments explaining what's being tested should use spec / language vocabulary, not "the `<crate>::<module>::<fn>` walker."
- Case-owned code must not directly read, copy, grep, patch, import, or native-compile a generated host-backend file. Review every case reference to `workdir/out`, target output paths, host-language extensions, or native compiler commands. Passing an output directory opaquely to a fixed runner protocol is allowed; the runner owns ordinary artifact loading and compilation.
- Kio' / Kio-prime is the explicit exception because it is a specified backend-neutral phase artifact. A golden may read or assemble Kio' when that artifact, its grammar/round trip/verifier/evaluator, or the dynamic-load boundary is the subject. Generic harness-owned phase checks are also allowed. Neither allowance extends to host-backend output.
- Hits are findings.

Run the structural emissions guard before the manual judgment pass:

```sh
sh ci/checks/repo-lint/emissions-corpus.sh
```

A failure is a finding. The lint checks the conventional backend-first marker,
scratch-isolation, and target structure for the emissions corpus. Golden-owned
host-artifact consumption is a manual placement review: inspect the references
listed above in context, including the opaque-runner and Kio' exceptions. Manual
review also owns whether an asserted host surface or artifact fact is
independently authored and durable.

## 3. Test placement

`TESTING.md` has the decision tree for where a new test goes (kio-rs unit test vs golden vs generative vs check). Walk recent test additions (`git log --since="3 months ago" --diff-filter=A --name-only -- 'test-data/' 'ci/infra/' 'kio-rs/'`) and check each landed in the right place:

- Implementation-detail tests → kio-rs unit tests (under `kio-rs/src/**/tests` or `kio-rs/tests/`).
- Language-behavior fixtures → `test-data/goldens/`.
- Generated public host ABI → first use a fixed independent runner protocol and cross-implementation golden. Only a backend-specific public-host fact that protocol cannot naturally prove belongs in one `HOST_INTERFACE` case under `test-data/emissions/<backend>/<case>/`, with a public-spec host determined independently from emitted output (normally fixed source; only mechanically-wide repetition may come from fixed checked-in parameters). Filesystem facts use `ARTIFACT_SHAPE` only when durable and spec- or recorded-measurement-backed.
- Private generated invariants that do not require a filesystem → kio-rs unit or mutation tests. An incidental private artifact assertion with no durable contract or measurement role is deleted, not migrated.
- Differential / property-based → `ci/infra/kio-gen-rs/`.
- Per-case assertion logic → `ci/checks/per-case/`.

A misplaced test is a finding.

## 4. Generator surface coverage

`ci/infra/kio-gen-rs/` is a differential generator — it should track the actual surface. Compare against `specs/grammar.md`:

- Productions in the grammar that the generator doesn't emit are coverage gaps.
- Productions the generator emits that the grammar doesn't include are bugs (the generator has fossilized against an older grammar).

Run `ci/infra/kio-gen-rs/` briefly if the working tree builds it, and spot-check the generated programs against the current grammar.

## 5. kio-prime gate

ai/topics/implementation.md: "The kio-prime binary is wired into the CI golden runner as a separate `--impl-def=` over `IS_KIO_PRIME`-marked cases." Verify:

- Each `IS_KIO_PRIME`-marked golden parses successfully under `kio-prime`.
- No `IS_KIO_PRIME` case contains a surface-only form (that would be a contradiction).
- Cases that exercise surface-only forms are NOT marked `IS_KIO_PRIME`.

## 6. Strategy redundancy

`TESTING.md`'s premise is that each test layer catches what the others miss — hand-written goldens pin cross-implementation language/diagnostic/runtime behavior, backend-first emissions independently pin public generated-host interfaces or durable artifact facts, generative tests fuzz the surface, the `fmt_*` golden corpus locks formatter idempotence and canonical form, the highlight-corpus pins lexer-level tokenization, per-case checks add ad-hoc assertions, and the POC corpus (`test-data/poc/`) exercises whole-library ergonomics + `equiv` law batteries that no other layer covers. The audit verifies that promise: every load-bearing behavior is exercised by at least one layer, and behaviors warranting more than one cover are not silently single-covered. Emissions are supplemental evidence and are never counted as a runtime backend-completeness cell.

Formatter canonicality is covered by the per-construct `fmt_*` golden family (each construct has its own small golden, so per-PR drift in a construct's canonical form is visible directly) plus `ci/checks/per-case/fmt-canonical.sh`, which asserts every tracked `src/` `.kio` file is at `kio fmt`'s fixed point each PR. When § 1's `specs/style.md` pass turns up a canonicalization rule with no exerciser in the `fmt_*` corpus, that is a single-layer / zero-layer gap to flag here.

Kio-family file-kind parity is a cross-layer contract. Enumerate the source of truth in `kio-rs/src/file_kind.rs` and `KioFileKind` in `kio-rs/src/ast.rs`, then verify each file kind is covered by `kio fmt` routing/tests, `kio doc` `variant=KIND` parser tests, `specs/cli.md` / `specs/kiodoc.md` / `specs/style.md` where applicable, `kio debug tokens` plus `test-data/highlight-corpus/`, TextMate/tree-sitter grammars, LSP formatting tests, and VS Code extension fixtures. A hard-coded module/package subset in any of those surfaces is a finding unless the file kind is explicitly irrelevant to that surface and the reason follows from the surface boundary.

Walk the layers and look for behaviors that fall through:

- **Single-layer coverage on a behavior that warrants more.** A spec-anchored behavior exercised by exactly one hand-written golden (with no generative coverage) is fragile — the golden could be deleted in a refactor and the regression would be invisible to CI's generative half. Flag as a candidate for adding generative coverage.
- **Layer visibly silent on its concern.** Walk recent codegen-touching commits in both directions: a new language/runtime or public-interface path needs a cross-implementation end-to-end golden through a fixed runner protocol. A public-host fact the runner cannot naturally and independently demonstrate needs a `HOST_INTERFACE` emission; a durable artifact claim needs an `ARTIFACT_SHAPE` emission or an equivalent harness-owned global check; and a private invariant needs unit/mutation coverage. An emission duplicating runner evidence is misplaced, while evidence for a genuinely different claim does not discharge another layer.
- **Strategy ↔ corpus shape mismatch.** `TESTING.md`'s decision tree says "new error category → golden in the matching `<NN_>` bucket." If a recently-added category landed without a bucket golden, that's a strategy violation independent of whether the spec is covered.
- **POC corpus's unique coverage missing.** The POC corpus is the only place that exercises (a) `equiv` law discharge at library scale and (b) whole-library API ergonomics under `kio check` + `kio test` + per-backend run. If a recently-landed surface feature with a real algebraic identity (a new combinator, a new prelude-shape primitive) doesn't have a corresponding `equiv` block somewhere under `test-data/poc/`, the redundancy promise is partially missing for that feature — flag as a candidate POC addition.

Each finding here is "this should be covered by more than one layer; today it's covered by only one (or zero)." Distinct from § 1 (which asks whether a spec feature has any coverage at all) and from § 4 (which asks whether the generator's emission surface is complete). POC-corpus shape, header-comment, intrinsics, operator/fn/equiv counts, and demonstrative-`main` discipline live in [`audit-corpus`](../audit-corpus/SKILL.md)'s POC section, not here — this skill defers to it for the POC contract.

### Runner independence

The redundancy promise has a precondition the audit also enforces: the test
runner must be an **independent** checker of the emitted artifact, not a second
copy of the emitter's knowledge. A runner that reads `kio build` output to
learn the package's host declarations, signatures, or structural shapes is
*backwards coupling* — it re-derives what the emitter already decided, so
emitter↔runner agreement becomes tautological and an emitter drift can no
longer surface as a runner failure. The runner reconstructs the complete host
interface from the selected **named protocol** documented in
[`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md);
the host compiler is the independent check that this fixed protocol contract
matches the emitted interface.

“From the protocol” means from the selected **named protocol itself**, not
from semantic metadata attached to an individual case. Any exact declaration
inventory needed to synthesize a typed host—including host-type module/name,
arity, role constraints, and the test host's chosen fixture
representation—belongs to that named protocol and is fixed for every case
that selects it. A case may select a protocol; it must not extend or repair the
protocol by passing a case-specific host-type inventory in `run.args`, another
sidecar, or ambient state. If two cases require different typed host
interfaces, sharpen or split the protocols (or use a genuinely compile-only
route) rather than making the runner adapt to each case. Both adaptive
directions are findings:

- **producer-derived adaptation** — reading generated source, comments,
  descriptors, reflection output, manifests, or interface declarations to
  discover the inventory or choose fixtures; and
- **case-derived adaptation** — accepting per-case semantic declarations or
  fixture choices that should instead be part of the named protocol.

Explicit artifact addressing is not semantic interface adaptation: the
harness may supply the source package name as the input to the default
namespace rule, and `run.args` may carry a target-id-qualified namespace
override already present in the package build contract. The harness selects
the current target's value; the runner independently applies the specified
namespace/brand derivation and treats the resulting effective namespace—not
the package name—as backend artifact identity. It does not inspect emitted
source to rediscover those inputs.

Concretely, the runner crate reads **no** `.kio` / `.pkg.kio` source and does
not inspect emitted interfaces, manifests, or source files to discover
semantic interface facts or package identity. Ordinary artifact loading and
generic content-addressed cache hashing remain allowed. Detection is a grep
over
[`ci/infra/kio-test-runner-rs/src/`](../../../ci/infra/kio-test-runner-rs/src/)
for reads of known interface/manifest paths or deleted manifest helpers:

```sh
grep -rnE 'host_descriptor|host_shape|host_api_from_descriptor_json|validate_against_manifest|read_crate_name|extract_shape_ident|find_host_shape_manifest|host_api_from_manifest' ci/infra/kio-test-runner-rs/src/
grep -rnE 'read_to_string|fs::read|\.join\("(Cargo\.toml|host\.rs|lib\.rs|shapes\.rs|ffi\.rs)"\)' ci/infra/kio-test-runner-rs/src/
```

Findings to flag:

- Any match naming `host_descriptor.json` / `host_shape.json` (the retired sidecars), or any of the deleted manifest helpers — a regression toward the old coupling.
- Any `fs::read` / `read_to_string` of `host.rs`, `lib.rs`, `shapes.rs`,
  `ffi.rs`, `Cargo.toml`, or another emitted interface/manifest/source file to
  discover declarations, signatures, shapes, package identity, or fixtures.
- Any generated comment, associated-type/member declaration, or other emitted
  interface fragment used to discover a typed host declaration inventory,
  recover its arity/role, or select the runner's fixture representation.
- Any `run.args`, sidecar, or environment input through which one case adds
  host types or fixture choices to a named protocol. The protocol inventory
  itself is the authority; a case selects it whole.

Some reads are **not** findings, by their nature:

- The JS runner reads the emitted `*.js` package module because that module *is* the executable it evaluates (analogous to rustc reading `lib.rs` as the crate root).
- The Rust runner's rlib cache hashes the emitted `src/` tree generically to key its build cache (`collect_crate_files` in `rust/rlib_cache/key.rs`): a content-addressed cache key, not interface introspection — it walks `src/` and reads whatever is there, never naming `lib.rs` / `shapes.rs` / `ffi.rs` / `host.rs` to learn the interface, and it forces a fixed `--crate-name` so it needs no `Cargo.toml` read.
- The Rust runner passes `src/lib.rs` to rustc as the crate root (`do_rustc_rlib` in `rust/rlib_cache/mod.rs`): handing the compile root to the compiler, not an `fs::read` of build output.

Referencing an emitted shape through the `ffi` boundary-alias module
(`crate::ffi::env|exp::…`) is likewise fine — that is a stable published name,
not a scrape of structural spellings. A grep hit is a finding only when the
runner itself reads a build-output file to learn semantic interface or
identity facts, distinct from compilation, execution, and generic cache
hashing.

Emission independence is the complementary check. A `HOST_INTERFACE` case has
one fixed checked-in `host/` authored from `specs/backends/<backend>.md`; it
does not generate, extract, or repair that host by reading the current output.
The ordinary host compiler/runtime may import, compile, link, or load the
generated public artifact. An `ARTIFACT_SHAPE` case may inspect the output, but
only to assert the exact public-spec or recorded-measurement fact named by its
`# SUBJECT:` / optional `# CONTRACT:` header. It must not pin private helper
names, formatting, or declaration order. Verify exactly one empty marker per
case, `host/` required only for `HOST_INTERFACE`, and no marker registry.

## 7. Run the suites

Run `sh ci/checks/orchestrators/golden-tests.sh --all-cases`, `sh ci/checks/orchestrators/emissions-tests.sh`, and `sh ci/checks/orchestrators/generative-tests.sh` if the working tree supports it. Explicit all-case coverage includes the otherwise sampled dynamic-Prime pass; do not add conflicting global/root/leaf case policies or raw harness sampling overrides. Direct emissions run all cases by default; case sampling is not an audit substitute. Any failure is a finding (separately from coverage gaps).

## 8. Backend consistency

Kio commits to identical semantics across configured backends: a case that builds for `target js` and `target rust` must run with identical observable behavior under both. The audit catches places this commitment isn't being honored:

- **Cases whose backend build or runner path declares only one backend.** A golden is cross-implementation language/diagnostic/runtime evidence; backend-specific generated-file inspection or native host compilation is never a reason for a single-backend golden and must move to `test-data/emissions/<backend>/`. Apply this check to cases that load an artifact, run a backend adapter, or otherwise make a backend claim. A compiler/front-end/CLI case is inherently implementation-level rather than backend evidence when it uses no target at all, or uses one representative routing target only to reach a shared compiler phase or debug probe and neither consumes the generated artifact nor asserts backend behavior; a Kio'-phase case may likewise be inherently tied to that explicit phase target. Verify that classification from the actual script and assertions instead of multiplying the identical compiler command across backend rows. For the remaining `00_success/` goldens whose build block omits a backend, classify each as (a) impossible-and-three-site-cited (admissible for an already-shipping backend), (b) misplaced generated-host evidence (move to emissions), or (c) emitter bug (add the backend once fixed). A `run.sh` comment alone is not a caveat, and a caveated new backend does not pass normal `add-backend` acceptance.
- **Per-backend carve-outs in the check pipeline.** A `[ "$KIO_TARGET" = js ] || exit 0`-style guard inside `ci/checks/per-case/*.sh` is a sign the check is implicitly JS-coupled; the right shape is `$KIO_TARGET`-parameterized end-to-end (the kio-prime-roundtrip check is the worked example).
- **Per-backend behavioral divergence.** A case that produces different stdout/stderr/exit under different `target=` impls violates the consistency commitment. The harness's divergent-cases table at end-of-run surfaces this; any divergent case is a finding (either the case is genuinely backend-specific and should declare only one backend, or one emitter is wrong).
- **Per-backend FFI surface parity.** When `specs/backends/<lang>.md` names an FFI shape (polymorphic host function, higher-rank parameter, existential, brand-based HKT, …), every applicable configured backend needs the cross-implementation runtime/public-interface golden through a fixed independent runner. Add backend-local `HOST_INTERFACE` evidence only for a public-host fact that runner cannot naturally demonstrate; redundant emission evidence is a placement finding. A missing runtime backend is admissible only under the required three-site caveat, and a backend-first emission never excuses it.
- **Runner-protocol parity.** [`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md) documents the exact named protocols mirrored by `RunnerProtocol` and `RunnerProtocol::contract` in [`ci/infra/kio-test-runner-rs/src/shared/protocol.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/protocol.rs). Walk source and README and assert every accepted name resolves to one complete structured contract, every documented name resolves, and distinct names do not silently alias the same contract. For each contract, verify exact qualified host-type and host-function identities, signatures, fixtures, export driver, and execution mode. A test may select a protocol whole; it may not supplement the contract through case metadata or artifact inspection. Every `ffi_*` golden selects the exact protocol that exercises its boundary; custom `run.sh` scripts must do the same explicitly. A protocol case carrying `SKIP_KIO_PRIME_RUN` needs a case-header reason why generic replay cannot reproduce its host setup, streams, or other runner details.
- **Canonical-host-fn index parity.** [`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md) lists the canonical host-fn shapes the test runners provide default bodies for. The index mirrors `CanonicalKind` in [`ci/infra/kio-test-runner-rs/src/shared/canonical.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/canonical.rs). Walk both and assert one row per variant — a variant in source without a README row, or a README row without a backing variant, is drift. The README's signature column must match `CanonicalKind`'s doc-comment signature exactly.

For each finding, cite the file and explain whether the gap is a corpus-authoring choice (case is genuinely backend-specific), an emitter gap (one backend can't reach what the other can), or a check-pipeline asymmetry.

## How to report

Group findings into:

1. **Spec features without test coverage** — the most important; user-visible commitments unexercised.
2. **Golden / emission hygiene violations** — implementation-internal golden names, direct golden-owned host-artifact handling, adaptive hosts, invalid markers, or incidental `ARTIFACT_SHAPE` assertions.
3. **Test misplacement** — case in the wrong layer per TESTING.md, including generated-host evidence in goldens or private invariants in emissions.
4. **Generator drift** — `kio-gen-rs` and `grammar.md` out of sync.
5. **kio-prime gate failures** — cases marked `IS_KIO_PRIME` that contain surface-only forms or fail under `kio-prime`.
6. **Single-layer coverage / strategy redundancy gaps** — behaviors that should be covered by multiple layers but aren't, language/runtime paths the cross-implementation runner isn't exercising, missing independent public-host evidence, or decision-tree-mandated coverage that was skipped. Emissions never close runtime coverage gaps.
7. **Backend inconsistency** — cases declaring only some of the backends they could portably support; per-backend carve-outs in the check pipeline; divergent stdout/stderr/exit across `target=` impls.
8. **Test-suite failures** — concrete failing cases from the suites.

For each finding, cite the spec section or test path.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

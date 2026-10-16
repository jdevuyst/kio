# kio-gen

Typed generator + differential test harness for Kio. A private Rust crate (`publish = false` in `Cargo.toml` — a test artifact, not a public surface) that emits a seeded batch of valid and labeled-invalid Kio test cases. Failures uncovered here get hand-promoted into goldens under [`test-data/goldens/`](../../../test-data/goldens/).

For how kio-gen plugs into the wider tests harness, see [`test-data/README.md`](../../../test-data/README.md). For how to run it, see "Quick start" below.

## Quick start

```sh
# From ci/infra/kio-gen-rs, generate a batch into a directory of your choice.
sh ../../cargo.sh run --release --bin kio-gen -- --output ./batch --count 500
```

The CLI is `kio-gen --output <dir> [--seed N] [--count N] [--no-shrink] [--prime-only]`. With `--prime-only` the generator restricts emission to Kio' (skips every surface form); without it, the default is full Kio.

## Design

Independent of `kio-rs` by construction. The generator builds typed terms in its own AST and renders them to Kio source text — it does *not* import the kio-rs AST. That independence is what makes it credible as a cross-implementation oracle: when implementations disagree on a generated program, neither side can blame a shared input.

Naming follows the `-rs` convention from [`kio-rs/`](../../../kio-rs/): this is the Rust-written generator.

## Output format

Each generated case is a directory mirroring the case-directory shape `ci/run-tests.sh` already consumes:

```text
<batch>/<NN_category>/prog_<n>/
  workdir/                     # directory where the case commands run
    prog.pkg.kio               # five targets; invoked control probes declare all eight
    prog.kio                   # module prog; generated host mirror + helpers + fn gen(...)
    *.kio         # elaborator support files in surface-mode cases
  run.args                     # exact generated core, surface, control, or recursion protocol
  run.sh                       # valid I32 results: capture execution, then kio test
  expected.stdout              # independently modelled control trace, otherwise empty
  expected.exit                # single integer exit code
  expected.stderr.ignore       # marker — error-message wording is impl-specific
```

`<NN_category>` matches a row in [`specs/exit-codes.md`](../../../specs/exit-codes.md) — `00_success`, `11_parse_error`, `12_import_error`, `13_name_resolution_error`, `14_type_error`, `15_elaborator_error`. Generated cases ship `expected.stderr.ignore` because the generator has no portable stderr oracle (kio-rs's wording is implementation-specific). A generated case ships `IS_KIO_PRIME` exactly when its program is Kio'-shaped end to end: a non-surface-mode program, and for mutants only a mutation that preserves the Kio' grammar. Surface-mode cases (elaborators, operators, other sugar) do not carry it. The `host` mirror declarations are ordinary Kio' items and do not disqualify a case.

The batch directory also gets a `README.md` describing the seed, the generator version, the flag set, and an exit-code distribution summary.

## Encoding the oracle

For invalid programs, the oracle is the expected exit code (in `expected.exit`); stderr wording is ignored. For valid programs, the harness discharges surface-mode `equiv` claims, builds the selected target, invokes `prog.main`, and compares stdout and exit against the independent expectations.

`generated-core-main` supplies the fourteen role types in `prog` and the
standard `prog.print_i32` capability; `generated-surface-main` adds the four
role types in the bundled `testapi` module. Valid functions returning `I32`
are closed with the existing typed inhabitant construction and invoked through
one shared `kio_gen_observed` function. Their `run.sh` captures the small
nonnegative integer, validates it, and appends an ordinary `equiv` comparing
that same call with the observed literal in an isolated source copy. The full
CLI's `kio test` reduces the generated computation, including lambda, product,
choice and polymorphic flows. Host arithmetic remains opaque. Core comparison
cases retain `IS_KIO_PRIME`; `SKIP_KIO_PRIME_RUN` keeps their surface test command
on the full CLI while retaining the source grammar and applicable phase checks.
The emitted source and script can be promoted together without the generator.
Other return types keep the existing runtime oracle.

Surface-mode valid cases also carry an invoked finite `ControlProbe`, selecting
`generated-control-main`: the same eighteen role types and integer printer plus
the standard `testapi/io.print` capability. Its independent schedule/value model
computes receiver, action, bind, eager product and explicitly delayed
thunk/sequence traces, including selected string values. The comparison script
preserves this trace for the harness's existing assertions. The renderer does
not compute expectations or interpret the general generated AST.

The control domain includes three sequence receivers, zero through three bind/action steps, lexical lets/references/shadows, bounded nesting, and all nine ordered product/thunk/sequence descriptor pairs. Probe cases declare JS, TS, Python, Java, Rust, Go, Swift and Haskell targets. Invalid cases, including unshrunken mutations, drop the probe; `--prime-only` never creates it. Seeded semantic distribution assertions print first witness seeds, while literal trace tests, deterministic source/stdout/protocol checks and actual cross-backend execution provide independent coverage. Existing handwritten control goldens remain unchanged.

Each sampled control schedule also carries an invoked finite type probe:
staged universal application, a higher-rank function argument, higher-kinded
projection through either of two constructors, or existential packing and
opening. Existential payloads pair a hidden seed with its `Hidden -> String`
observer; the opening invokes that observer and returns its result. The seed
varies across a scalar, a product with distinct selected fields, and a nominal
wrapper. Opening varies between explicit CPS and existential `let` syntax.
Sequence probes pass their actual selected value through the type probe;
block probes supply a sampled string after observing their blocks. The type
probe's result is printed and passed into any recursive probe, whose returned
value is also printed.

Optional finite recursion probes select `generated-recursion-main`, which adds
the standard `testapi/iter.loop` and `testapi/iter.loop_reentry` capabilities to
the control contract. Fuel ranges from zero through three. Singleton and mutual
functions exercise tail calls, pending work, changed type arguments with and
without pending work, and escaped
callbacks, including changed type arguments across an escape. Escaped callbacks
run after the creating call returns. An independent fuel-and-mode trace records
every step, base case, pending continuation, and selected value.

## Cross-implementation agreement

`ci/run-tests.sh` natively handles multiple implementations via repeated `--impl-def=` flags. Its [command reference](../../run-tests.sh) defines the implementation-descriptor schema. The script asserts every impl matches `expected.stdout`, `expected.exit`, and the case's stderr policy, and prints a divergent-cases table when impls disagree.

Cross-implementation agreement happens by **transitivity through a precise oracle**: if every impl matches its `expected.*`, they all agree. If two diverge, the table shows which case and which impl.

This design assumes the oracle is **point-precise**. Both goldens and kio-gen output meet that bar (goldens are hand-written; kio-gen knows the type by construction and labels the mutator's expected exit code). If kio-gen ever introduces cases with a *class* of acceptable outputs, transitivity-via-oracle no longer suffices and a separate cross-impl diff tool would be needed — but that lives outside `run-tests.sh`.

## Strong oracle for invalid programs

The mutator labels each corruption with the error category it intends to fire (parse / import / name-res / type / elaborator) and asserts the implementation rejects with the matching exit code from [`specs/exit-codes.md`](../../../specs/exit-codes.md). Two reasons to do this strong-from-start:

1. It catches **generator** bugs: a "should be type error" that silently parses-and-runs is a generator failure, not an implementation failure.
2. It enables the exit-code coverage check below.

## Exit-code coverage

Layer (2) of the self-tests asserts that the corpus hits every **coverable** exit code at least once. The discipline this enforces: when a new code lands in [`specs/exit-codes.md`](../../../specs/exit-codes.md), layer (2) fails until either (a) a matching mutator lands, or (b) the new code joins the non-coverable list below with reasoning. Keeps spec and test surface in sync.

**Coverable codes today: 0, 11, 12, 13, 14, 15.**

- **0 (success)** — every valid program; trivially hit.
- **11 (parse error)** — mutator corrupts source syntactically (a stray `@`).
- **12 (import error)** — mutator imports an unexported item from a helper module.
- **13 (name resolution error)** — mutator wraps the body in a `let` referencing an unbound name.
- **14 (type error)** — mutator wraps the body in a `let` whose RHS has the wrong type.
- **15 (elaborator error)** — mutator appends a top-level `defn` whose body is `into!("…")` checked against an `I32` return.

**Non-coverable:**

- **1 (internal error)** — unreachable from well-formed input; if the generator triggers it, that's an implementation bug, not coverage signal. (This was one of the value propositions in practice — see "Bugs found" below.)
- **2 (CLI usage error)** — about how `kio` is invoked, not Kio source.
- **10 (non-specific compile error)** — the mutators isolate a more specific compile category.
- **20 / 40 (bridge / build error)** — the mutator catalog changes module source inside an otherwise valid fixed package and build contract.
- **16 (totality error)** — the catalog preserves positive recursive types and structural recursion. Non-exhaustive and unreachable `match!` clauses belong to elaborator category 15.

## Surface coverage

**The generator's scope is the entire Kio surface language.** Every
production in `specs/language.md` and `specs/grammar.md`'s surface
section is meant to be generable, with layer (2) of the self-tests
below asserting each form appears in the rendered text — the same
discipline as the exit-code coverage check above. The bar is the
same as for exit codes: when a form is added to the surface and the
generator doesn't grow to cover it in the same change, the
distribution test should be the thing that fails.

The generation inventory combines freely composed typed terms with bounded
invoked probes. Its distribution assertions cover the following production
cells; fixed support declarations supply their explicitly described helpers:

- Tuple literals `(a, b, ...)`.
- `if`/`else` conditionals.
- Label declarations (`labels`) and label-value sugar `{label = e}`.
- Field updates and row lets use a seeded three-field record probe: source
  field order, replacement subsets and values, selected bindings and aliases
  vary independently. The oracle checks receiver evaluation, replacement
  order, updated and untouched payloads, and named binding results through
  the existing control protocol.
- `match!` with `.(p: T) { ... }` clauses.
- The 17 surface elaborator forms — emitted as identity coercions
  (the generator picks the surface form; the Kio' fallback
  elaborates to the inner expression for all of them, since
  identity is admitted by every elaborator). The emission policy is
  **primarily spine, low rate of algebraic** (per
  [`docs/guides/using-libraries.md`](../../../docs/guides/using-libraries.md)):
  within the wrap subset (~10% of expressions), the per-subset
  roll is 80% spine / 20% algebraic. This makes the spine palette
  the everyday material while keeping the algebraic codepath
  exercised on every batch.
  - **Spine palette** (11 forms): `reorder_sum!`, `reorder_prod!`,
    `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`,
    `flatten_sum!`, `flatten_prod!`, `one_sum!`, `one_prod!`,
    `fit!`. Per-form applicability is gated by the source's
    outermost shape: `reorder_*` / `narrow_*` / `widen_*` /
    `flatten_*` require the matching axis (sum or product) at the
    top level (`flatten_*` identity-short-circuits whenever source
    already equals target); `one_sum!` / `one_prod!` only fire
    at atomic sources (the safe degenerate-identity case);
    `fit!` admits identity at every type and carries a higher
    per-form weight as the most general spine form.
  - **Algebraic palette** (6 forms): `iso!`, `into!`, `onto!`,
    `align!`, `ease!`, `atom!`. `atom!` restricts to atomic targets
    per the spec's single-arm rule; the other five admit identity
    at every type (the freely composed term domain uses monomorphic targets, so `ease!`'s
    polymorphic-source/target rejection never applies).
- UFCS — all four spellings, `.>`, `.>>`, `.<`, and `.<<`, with bare
  receiver-only forms and nonempty argument tails. Rewrapping a generated
  `PolyCall` or `App` removes the appropriate endpoint value and preserves
  the original call order in the core rendering. Polymorphic calls vary
  explicit and inferred type arguments. A finite staged probe places a type
  binder after the first value slot and observes the selected first value.
- Higher-rank function values, higher-kinded application, and nominal
  existentials use the invoked type-probe domain described above.
- Recursive function singletons and groups, plus `recur`, `recur(cont)`,
  `recur(poly)`, `recur(poly, cont)`, `recur(escape)`, and `recur(poly, escape)`, use the finite
  recursion-probe domain described above.
- `.arg.` placeholder lambdas — emitted at function-typed targets
  where one of the params has the function's declared return
  type. The body is the identity-on-that-slot `.arg. { argN }`. Kio'
  fallback renders the same shape as a regular `.(p1, ..., pN)
  { pN }` lambda (matching the surface desugar pass).
- `op` user-operator bindings — `pub op _ <+> _ { impl kio_gen_op_pick; };`
  is declared in `prog.kio` alongside generated code. Use sites in the body
  appear as the surface `lhs <+> rhs` chain (parenthesized to
  avoid the non-associative chain restriction on `_ <+> _`); the
  Kio' fallback renders the underlying call.
- Seeded variadic operators — every surface-mode package declares a
  right fold whose `foldr1 kio_gen_fold_step kio_gen_fold_seed;` consumes the terminal
  element. Separate fixed `equiv` claims prove that a singleton is not
  stepped and that the remaining element of a two-element fold is stepped.
- Literal aliases — `pub literal zero_i32 = 0;` and
  `pub literal hello_str = "hello";` in `prog.kio`, with
  use-site references appearing in surface mode as the annotated
  alias call and in Kio' fallback as the bound literal (the
  desugaring substitution).
- `equiv` claims — fixed Unit reflexivity, seeded-fold and label-forwarding
  claims are appended to `prog.kio` for every surface-mode case. The
  standard `run.args` path runs `kio test` before build/run; eligible integer
  cases discharge them together with the observed-result claim after execution.
- User-defined `elab` declarations — surface-mode cases generate a
  `kio_gen_elaborators.kio` module with 1-3 ordinary-schedule
  identity-style `pub elab` declarations plus one capture-free
  `impl(fills)` identity. Ordinary implementation bodies are randomized
  over a bounded reflected-ABI template catalog
  (`__either__` target requests, `__term_let__`, reflected product /
  sum round-trips, type guards, and optional captures), and generated
  bodies occasionally call the sampled ordinary names as `name!(expr, _)`.
  A fixed root-module function calls the marked identity, whose private ABI
  returns the checked term paired with `__fill__(ct, fills, target, source)`.
  Every generated implementation is a `pure fn`, matching the compile-time
  execution contract.

Surface-mode packages import those elaborators from same-package
root module files. The generator writes the POC elaborator support
files into each generated case that can mention a bang-call surface form.

## Generator self-tests

Three layers, in increasing cost and value. The first two run from this crate
directory as ordinary `sh ../../cargo.sh test` invocations:

1. **Self-consistency unit tests** (`tests/selfcheck.rs`). For every valid program emitted, an independent re-walk reproduces the type the generator claimed. All binders are in scope. Depth/size bounds are honored. No seed panics.
2. **Distribution / corpus tests** (`tests/distribution.rs`). Sample N programs and assert the mix isn't degenerate: hash-collision rate is low; depth histogram isn't all-shallow; every base type appears; polymorphism + the `__if_then_else__` intrinsic each show up at least once; **every coverable exit code is hit**; Kio'-body and surface-body renders are both represented (≥ 20% on each side); **every covered surface form appears in the rendered text at least once** (see § Surface coverage above); reserved-elsewhere identifiers (`class` / `default` / `def` / etc.) appear at least once.
3. **Coverage delta.** From the repository root, `sh reports/kio-gen-coverage-delta.sh` drives `cargo-llvm-cov` over the goldens harness twice — once on goldens alone, once on goldens + a kio-gen batch at the per-PR budget. Reports `kio-gen contributed +X.X% line coverage over goldens (seed=…)`. No thresholds, no gating; just kio-gen's marginal contribution as a delta that a maintainer can interpret. The interpretation half is local review: are the generator's productions still aligned with where the implementation has grown?

What we explicitly aren't building: a per-file coverage table with file-by-file thresholds. Those bit-rot fast and the report is really a strategic / direction-setting signal rather than a regression signal. The trend number gives visibility, the agent audit gives interpretation.

## Persistence

The generated corpus is **ephemeral**. The binary chooses a random seed unless `--seed N` is passed, and always logs the seed so any failure can be reproduced. GitHub Actions uses that default random seed behavior for its generated-case sweeps; only manually-promoted failures land in [`test-data/goldens/`](../../../test-data/goldens/).

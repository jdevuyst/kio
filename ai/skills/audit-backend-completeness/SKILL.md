---
name: audit-backend-completeness
description: Verify every spec-admitted value shape, polymorphic application stage, and deep recursive stack-safety witness is runtime-exercised on every backend — each boundary cell and internal witness actually runs; impossibilities remain visible caveats, and build-only / absent / stubbed cells are findings
allowed-tools: Read, Grep, Glob, Bash
---

# Per-backend completeness audit

AGENTS.md § Universal rules — Per-backend limitations: only the genuinely impossible, mutual-cited says a backend must implement **every** spec- and IR-admitted feature its host language can express, and that *the absence of a golden exercising a shape is not license to skip, stub, or silently identity-pass it — an unexercised shape is undetected, not unsupported, so the missing golden is itself the bug (write it).* The signal-based sweeps key off something written down: [`audit-partial-implementations`](../audit-partial-implementations/SKILL.md) keys off a comment phrase / error arm / `allow(dead_code)`, [`audit-spec-drift`](../audit-spec-drift/SKILL.md) § 7 keys off a carve-out paragraph, [`audit-corpus`](../audit-corpus/SKILL.md) keys off a deleted or per-backend-opted-out golden. **None of them fires on a shape that is simply absent from the corpus on a backend** — a shape that compiles, never crosses the boundary at runtime on that backend, and carries no comment, no error arm, no carve-out. That is the gap this audit closes.

The discipline is structural: enumerate the spec-admitted shape axes, cross them with the shipping backends and the two host-boundary directions, and for **each cell** demand a golden that **actually runs** the shape across the boundary on that backend. Separately, require the internal polymorphic application-stage and recursive control-flow stack-safety witnesses in § 4 for every backend. A mutual-cited genuine impossibility is recorded as an explicit caveat, not as a green cell. A cell or internal witness that is build-only (`kio check` / `construct-only`, no runtime crossing), absent, or stubbed — with no valid impossibility classification — is a finding. `test-data/emissions/` may independently prove a public facade or durable artifact fact, but it never satisfies or upgrades a runtime cell. This is the audit analogue of "an unexercised shape is the missing-golden bug": the audit makes *undetected* visible.

This is a **completeness** discipline, not a soundness one (like [`audit-partial-implementations`](../audit-partial-implementations/SKILL.md) § 7's evaluator framing). A missing cell never produces wrong output — it produces *no* output, so nothing fails loudly. The danger is exactly that silence: a shape that a backend cannot actually emit ships green because no golden ever asks it to, and the gap is discovered only by someone investigating by hand. The matrix is the systematic guarantee that replaces "we happen to know."

Read before starting: AGENTS.md § Universal rules — Per-backend limitations: only the genuinely impossible, mutual-cited, § Backend decisions propagate by semantic applicability, and § Bugs surface; never hide them; [`specs/backends/README.md`](../../../specs/backends/README.md) §§ Language families, Structural FFI shape conventions, Higher-kinded types, Polymorphic newtype payloads; [`ai/topics/emit.md`](../../topics/emit.md) § The escape hatch (the impossibility standard), § Decision record and propagation gate, and § Runtime model (the internal-vs-boundary-crossing distinction); and the protocol catalogue in [`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md) (§§ The protocol model, Exact main protocols, Roundtrip protocols). The shipping backend set and family table are read from `specs/backends/README.md` at run time — adding a backend or family is a spec edit, not an audit-code edit.

## 1. Enumerate the spec-admitted shape axes

The rows of the matrix are the value shapes / type-formers Kio admits at the FFI boundary. Ground each in `specs/`, not in a hardcoded list — when the spec gains a shape, this enumeration grows with it. The current axes and their spec anchors:

| Shape axis | Spec anchor | What crosses |
| --- | --- | --- |
| **Scalars / role types** | `specs/language.md` § The host boundary (role-assigned host types: the sized numerics, `bool`, `str`); `specs/backends/README.md` § 8 Host-trait descriptor (role-binding layer) | Atomic host-owned values — `I32`/`I64`/…/`U128`, `F32`/`F64`, `String`, `Bool`, wide ints |
| **Product** (`&`) | `specs/language.md` § Anonymous sum and product types; `specs/backends/README.md` § Structural FFI shape conventions, § The right-spine walk | A right-spine-walked n-slot product value |
| **Sum** (`\|`) | `specs/language.md` § Anonymous sum and product types; `specs/backends/README.md` § The right-spine walk | A single-keyed n-slot sum value (which arm) |
| **Newtype** | `specs/prime.md` § The `newtype` primitive; `specs/language.md` § Type declarations, § Labels | A nominally-distinct payload through its constructor/projector, incl. `labels { }`-generated newtypes |
| **Closure / function value** | `specs/backends/README.md` § 2 Type erasure at the FFI, § Function-type FFI canonicalization; `specs/language.md` § Function types, § Type parameters (curry-layer rule) | A function value crossing as a callback — **both directions** (host fn passed to a Kio export; Kio fn returned to / called by the host). The **leg type is part of the cell**: a concrete-leg callback does not exercise a *type-variable-typed* leg (an erased→typed boundary conversion the erased backends must emit — rust `from_any::<K>`, Go erases to `any`) nor a *function value held in a product / sum slot* (recovered by the slot's grouping, not its flat per-group arity); each is its own running golden (`exec_host_closure_type_param_leg`, `exec_fn_value_in_product_slot`) |
| **Multi-group / curried closure** | `specs/backends/README.md` § Function-type FFI canonicalization ("curried multi-layer functions stay curried"); `specs/language.md` § Type parameters | A curried `A -> B -> C` value whose curry layers stay distinct at the boundary, vs. a right-folded product-domain group |
| **Rank-N polymorphism** | `specs/language.md` § Type parameters (rank-N value params, `f: [T](T) -> T`); `specs/backends/README.md` § 2 Type erasure at the FFI | A polymorphic function value with its type-arg slots erased at the boundary |
| **Existential** | `specs/prime.md` § Existential introduction and elimination; `specs/language.md` § Existential type binders; `specs/language.md` § The host boundary (opaque `host type`) | A CPS-projected existential-bearing newtype value; an opaque host type (`host type Token;`) |
| **Host-owned generic type** | `specs/language.md` § The host boundary; `specs/backends/README.md` § 8 Host-trait descriptor | A host-supplied parametric type carrier — `host type Array[T];`, `host type Box[T];` |
| **HKT carrier** (kind-`*→*`) | `specs/prime.md` § Higher-kinded types; `specs/language.md` § Higher-kinded types; `specs/backends/README.md` § Higher-kinded types | A higher-kinded newtype application (`F(A)`) lifted/projected across a kind-`*→*` boundary — **internally** within package code (see § 4) |
| **Functor / monad dictionary** | `specs/language.md` § Higher-kinded types, § The `derive!` elaborator; `specs/backends/README.md` § Polymorphic newtype payloads | The `newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B)` dictionary value (`mk_monad`/`bind`, `mk_functor`/`map`) — an ordinary `fn`-parameter value, **never a host typeclass instance, on every backend** |

Host-representation injectivity is not another runtime-matrix axis. The
scalar / opaque-host row requires values of exact declarations to cross; it
does not require distinct host representations or a new runtime golden solely
to prove that two declaration entries may share one representation. Check
that admissibility with the fixed runner's public-interface evidence and, only
when it cannot naturally demonstrate the fact, a facade-appropriate
`HOST_INTERFACE` emission; unit/mutation or spec evidence may also apply in
`audit-spec-drift` and `audit-runner-host-fidelity`.

The HKT axis splits into two distinct cells that must not be conflated — this is the load-bearing distinction the audit exists to keep honest:

- **HKT-internal** — the dictionary / higher-kinded carrier is constructed and consumed *within* package code (a `do`-block, a transformer stack, a `derive!`-built instance threaded as a `fn` value). It never crosses the FFI as a typed host shape. Per `ai/topics/emit.md` § Runtime model, this is what every family realizes today (the erased-static backends — Go, Rust, Swift — ride the carrier on the universal erased rep with no carrier-walk, the worked 3-layer transformer stack included; Haskell threads the rank-N field directly).
- **HKT-boundary** — a `host fn` whose **parameter or return type is itself a Functor/Monad dictionary** (or a higher-kinded carrier as a typed boundary shape), so the dictionary value crosses the host boundary. **The spec does not require this** (`specs/backends/README.md` § Higher-kinded types is explicit: the dictionary is "an ordinary value Kio passes as a `fn` parameter — never a host typeclass instance — on every backend"). It is therefore *admissible but not mandated* — and it is runtime-covered: `ffi_host_functor_dict_roundtrip` (protocol `host-functor-dict-roundtrip`) crosses a `Functor[*F]` dictionary as both the parameter and the return of `round_functor`, on all shipping backends. Whether to exercise a given HKT-boundary shape is a coverage decision, not a conformance one — but an *uncovered* one is still exactly the "undetected, not unsupported" condition, and the missing golden is the bug to write (§ 3 fail condition, with the § 5 caveat for genuinely-not-mandated shapes).

## 2. Enumerate the backend × direction columns

The columns are `(backend × direction)`:

- **Backends** — the shipping backend list from `specs/backends/README.md` § Backend pages. Read it at run time; do not hardcode. Record each backend independently. Group rows only after applicability, result, and evidence agree; family membership alone never discharges a cell or an impossibility claim.
- **Directions** — the two host-boundary directions the FFI contract distinguishes (`specs/backends/README.md` § 8 Host-trait descriptor; `specs/package.md` § The bridge block):
  - **host→package (env)** — a `host fn` the package *calls*; the shape crosses as a host-fn parameter or return.
  - **package→host (export)** — a `pub` item the host *calls*; the shape crosses as an export's parameter or return.

A shape's coverage is per `(shape, backend, direction)`. Many shapes have a natural home in one direction (rank-N value params and host-owned generic types are env-side; exported polymorphic functions are export-side), but products / sums / newtypes / closures cross **both** ways and need both cells covered.

## 3. Define the per-cell check and the fail condition

For each `(shape, backend, direction)` cell, the cell is **green** iff it is runtime-exercised:

**Runtime-exercised** — there is a golden (or POC case) that **actually runs** the shape across that direction's boundary on that backend. "Actually runs" is the hard part of the definition:

- The golden's `build { ... }` block lists that backend, **and**
- the case is driven by a protocol that *runs the emitted artifact* — `empty-main`, another main protocol, or a roundtrip protocol that calls an export (per `ci/infra/kio-test-runner-rs/README.md`). It is **not** `construct-only` and **not** a `kio check` / build-only golden (e.g. the `40_build_error/` and `14_type_error/` buckets, or a `00_success` golden with no runtime `run.args`/`run.sh` driver). Construct-only proves the *shape compiles*; it does not prove the *value crosses at runtime*.
- The protocol's env (host→package) or export-driver (package→host) actually moves the shape across the boundary. A roundtrip protocol whose driver builds the shape, hands it across, and reads it back is the gold standard; a tier whose canonical env contains the shape (e.g. `loop`'s `(S | R)` step crossing a sum, `read_ascii_line` / `array_pop_back` returning `T | .`, `array_*` over `Array[T]`) also counts.
- An emission case does not count, even when its fixed host compiles, links, loads, or calls the artifact. Emissions answer whether the generated public interface is independently usable or whether a durable filesystem fact holds; they do not provide cross-implementation language/runtime semantics and cannot turn build-only or absent runtime evidence green.

**Justified impossibility (`I`)** — a non-green cell may be classified as a concrete known caveat only when the host language genuinely cannot express it and the impossibility is stated in matching top banners in `specs/backends/<lang>.md` and `docs/hosts/<lang>.md`, with the detailed spec section named at the emitter degradation site. The impossibility standard is `ai/topics/emit.md` § The escape hatch — genuinely impossible only when the host has *no* escape hatch (no `unsafe`, no FFI, no unchecked cast). Discharge each claimed impossibility (§ 5). An `I` cell keeps the backend visibly caveated; it does not count as green and cannot pass normal `add-backend` acceptance.

**A cell that is neither runtime-exercised nor a valid, visibly bannered impossibility is a finding.** The three failing sub-shapes:

- **Build-only** — a golden exists and lists the backend, but runs `construct-only` or is `kio check`-only. The shape compiles but never crosses at runtime. Fix: drive it with a runtime protocol (add `--protocol` to `run.args`, or write a roundtrip).
- **Absent** — no golden exercises the shape on that backend at all. Fix: write the golden, with the backend in its `build { ... }` block and a protocol that crosses the shape.
- **Stubbed / silently identity-passed** — the shape reaches the emitter but is routed to an error stub, an identity pass-through, or a universal catch-all that no runtime golden distinguishes from a correct emission. (The `Custom` canonical error-stub — the backend's canonical failing construct, e.g. rust's `panic!` or js's `throw` — is the *correct* shape for a genuinely-uncalled user host fn; it is a finding only when it stands in for a shape a golden *should* cross but doesn't.) Cross-ref [`audit-partial-implementations`](../audit-partial-implementations/SKILL.md) § 6.

The fail condition is deliberately symmetric with the universal rule it enforces: a missing golden is not a neutral absence, it is the bug. The audit does not accept "this shape isn't exercised on backend X, but it probably works" — that *probably* is precisely the undetected state.

## 4. The internal-vs-boundary-crossing distinction (do not conflate)

A history-only compatibility stub is neither a live environment cell nor
runtime coverage. Do not count its declaration or direct host-side trapping
call as evidence for a boundary occurrence; any package dispatch to it is a
contract failure, not a covered cell.

A frequent false alarm — and the one a stray `(deferred)` comment can manufacture — is confusing **HKT-internal** coverage with **HKT-boundary** coverage. The audit must keep them as separate cells:

- The HKT-internal cell is covered when a golden builds and runs a higher-kinded program (a functor/monad dictionary, a `do`-block, a transformer stack) on the backend — the dictionary threads through *package code*. This is covered broadly today (`exec_functor_*`, `exec_monad_*`, `exec_do_block_*`, `exec_derive_transformer_stack`, the `poc/hkt` and `poc/dict` cases), on all shipping backends.
- The HKT-boundary cell is covered only when a `host fn`'s parameter or return type *is* a dictionary / higher-kinded typed shape. **Verify this by inspecting the host-fn signatures**, not by the presence of the word "monad" in a golden name. A golden named `exec_monad_dictionary` whose only `host fn` is `print(String) -> .` covers HKT-**internal**, not HKT-boundary — the dictionary is built and projected internally and only `String` scalars cross.

The grep that separates them: for every HKT-named golden/POC, list its `host fn` declarations and check whether any parameter or return type mentions a dictionary newtype or a higher-kinded application as a typed boundary shape:

```sh
# HKT/dictionary cases:
ls test-data/goldens/00_success/ | grep -iE 'monad|functor|dict|hkt|do_block|transformer|brand|carrier|app_'
# For each, does a host fn cross a dictionary / higher-kinded typed shape?
grep -rhnE 'host fn .*(Monad|Functor)\(|host fn .*-> *(Monad|Functor)\b' test-data/goldens test-data/poc
# Non-empty ⇒ HKT-boundary is covered by the matching case(s) — today,
# ffi_host_functor_dict_roundtrip's `round_functor`. Empty result would
# mean HKT-boundary is uncovered (the dictionary crosses internally only).
```

Crucially, per § 1, the HKT-boundary cell is **not spec-mandated** — `specs/backends/README.md` says the dictionary is an ordinary `fn`-value on every backend and never a host typeclass instance, so the spec nowhere requires a dictionary to be a host-fn parameter/return. It is covered regardless: `ffi_host_functor_dict_roundtrip` (protocol `host-functor-dict-roundtrip`) runtime-exercises the dictionary across the boundary on all shipping backends. Were some other HKT-boundary shape to go uncovered, it would be reported as a **coverage gap to fill (write the golden)**, with the § 5 not-mandated caveat — *not* as a conformance violation and *not* as a per-backend impossibility.

### Polymorphic application-stage runtime witness

The boundary matrix does not by itself prove that an erased body retained the
ordered application tree of an internal polymorphic value. For every shipping
backend, also identify running corpus evidence for all four shapes below:

1. **Adjacent direct binders** — a direct call with two or more adjacent type
   arguments advances the same number of unary `Forall` binders before its
   value arguments.
2. **Returned or computed polymorphism** — applying a singular
   `LowTypeApplication` to a value produced by an expression advances that
   returned callable rather than being folded into an unrelated direct call.
3. **Adjacent first-class binders** — applying two or more consecutive
   `LowTypeApplication` nodes to one returned, computed, or otherwise escaped
   value preserves and consumes each semantic binder/application boundary
   exactly once under the occurrence's applicable contract. The witness must
   distinguish preservation from dropping, reordering, or duplication without
   assuming one runtime call per binder.
4. **Observable boundary order** — observable work that produces a later
   polymorphic or value-call stage completes before that later application and
   its value arguments. The witness must distinguish correct order from a
   flattened callable.

One focused case may cover more than one shape, but build-only or
construct-only evidence does not count. Check each occurrence's emitted
realization as well as the runtime result. Host type abstractions, callable
stages, erased wrappers, and compaction are evidence to inspect, not outcomes
inferred from family or body representation. The realization must preserve the
current applicable facade and every semantic binder/application boundary.
Missing evidence is a completeness finding and calls for a focused runtime
golden, not an inferred claim from the emitter source.

### Recursive control-flow stack-safety runtime witness

The boundary matrix and polymorphic-stage witness do not prove that lowering
or emission preserves constant-host-stack tail recursion. For every shipping
backend, identify a running deep finite `rec(loop)` case whose state carries a
structured/product value and a continuation or other function-valued slot
through an elaborated branch. The runtime must complete without host-stack
growth proportional to the number of `Continue` iterations.

Pair that runtime evidence with a causal guard at the narrowest shared or
backend-local adaptation layer. A tail `Continue` that projects and reinserts
the exact carried function value already in the destination slot
representation, with no pending semantic conversion, must keep that value
without nesting another adapter. The guard must also prove all three
nonidentity boundaries: a same-ABI edge with real argument, result, nominal,
evaluation, or continuation conversion; a genuinely different function ABI;
and genuine pending continuation work such as `rec(cont)`.

The runtime evidence records one exact program, fixed iteration count,
expected structured result, and failure mode. It is causal only when that same
input runs GREEN on every backend. For each semantically distinct adaptation
owner or realization, the evidence is causal only when the input reaches
execution and fails from wrapper or host-frame growth with the wrapper-growing
behavior restored, then passes after restoration of the identity path; a
compile or setup failure earns no RED credit. One shared-layer
RED/restored-GREEN pair covers all consumers of that layer; each backend-local
owner requires its own pair. Backend-family labels and a passing shallow
recursion case are not evidence for this property. Missing runtime or causal
evidence is a completeness finding even when every boundary-shape cell is
green.

## 5. Discharge procedure — how an auditor verifies each cell

The matrix is built and discharged mechanically-then-manually, exactly as the carve-out discharges in [`audit-spec-drift`](../audit-spec-drift/SKILL.md) § 7:

1. **Build the row labels** from § 1 (re-read the spec anchors; add any new shape the spec admits).
2. **Build the column labels** from `specs/backends/README.md` § Backend pages × the two directions.
3. **For each runtime golden**, record which `(shape, backend, direction)` cells it covers. Mechanically:
   - Enumerate the runtime goldens: `00_success` cases with a runtime `run.args`/`run.sh` (a `-main` tier, a testapi protocol, or a roundtrip) — exclude `empty-main` and `kio check`-only cases. The `ffi_` / `exec_` prefixes are the bulk; confirm by reading `run.args` for the `--protocol`.
   - For each, read its `build { ... }` target list (`grep -oE 'target [a-z_-]+'` on the `*.pkg.kio`) and its protocol. The protocol determines which shape crosses and in which direction (the roundtrip table in `ci/infra/kio-test-runner-rs/README.md` § Roundtrip protocols maps each protocol to the shape it exercises; the tier table maps each tier to its env).
   - **Flag any runtime golden whose `build { ... }` omits a backend** that the shape's other goldens cover — a per-backend opt-out is the build-only/absent condition by another name (cross-ref [`audit-corpus`](../audit-corpus/SKILL.md) §§ 1–3).
   - Keep `test-data/emissions/` out of this enumeration. Record relevant emission evidence in a supplemental note, never in the runtime matrix cell.
4. **Mark each cell** green (runtime-exercised, citing the golden + protocol), caveated-impossible (citing both banners, the spec/emitter mutual-cite, and the discharge), or **red** (no runtime golden, no valid caveat).
5. **For each red cell, force the discharge question** — is the shape *runtime-exercisable on that host* (escape hatch present, `ai/topics/emit.md` § The escape hatch)? If yes, the red cell is a **finding**: the missing golden is the bug. If the host genuinely cannot express it (no escape hatch), classify it as `I` only after adding the matching concrete banners and spec/emitter mutual-cite; the backend remains caveated rather than green.
6. **For each claimed-impossible (already-carve-out'd) cell, re-discharge the impossibility** against the escape-hatch standard exactly as `audit-spec-drift` § 7 step 2 — a false-impossibility carve-out passes every signal sweep; only the manual discharge catches it. A `fixable` verdict is a finding even with the mutual-cite intact.
7. **Record the not-mandated caveat.** A shape the spec admits but does **not** mandate at a given cell is reported as a **coverage gap** — fill it with a golden — rather than a conformance finding, when it is uncovered. (The HKT-boundary cell per § 1 / § 4 is the standing example of this *category* — spec-admitted, not mandated — but that particular cell is itself covered: `ffi_host_functor_dict_roundtrip`.) Do not silently drop an uncovered one: an admissible-but-unexercised shape is still undetected. State explicitly, per cell, whether it is spec-*mandated* (red = conformance finding) or spec-*admitted-not-mandated* (red = coverage gap to fill). This is the one place the audit distinguishes "must" from "should"; everywhere else, red is a finding.
8. **Discharge the application-stage witness** from § 4 for every backend.
   Record the running case(s), the four shapes they distinguish, and the
   occurrence-appropriate emitted realization. A green boundary-shape cell does
   not excuse a missing internal-stage witness.
9. **Discharge the recursive stack-safety witness** from § 4 for every backend.
   Record the exact running case, iteration count, expected result, and restored
   GREEN on every backend. Per semantically distinct adaptation owner or
   realization, record the executing wrapper-growth RED and restored GREEN plus
   the exact-carried-representation/no-wrapper causal guard; one shared-layer
   pair covers its consumers, while each backend-local owner requires its own
   pair. Include same-ABI real-conversion, different-ABI, and
   genuine-pending-work controls. A shallow pass or compile/setup RED does not
   discharge the cohort.

The recorded matrix (the § How to report table) is the artifact. It is re-derivable each run; it is not checked in.

## 6. How `/add-backend` gates on this audit

This audit is the hard gate in the backend-completion contract: **a backend is not done until its column is fully green.** When the [`add-backend`](../add-backend/SKILL.md) skill prepares a new backend, it runs `audit-backend-completeness` as the final acceptance step and treats any red or `I` cell in the new backend's column as a blocking failure — the same status as a failing golden. Concretely, `/add-backend`'s definition-of-done includes:

- Every shape axis (§ 1) has a runtime-exercising golden whose `build { ... }` block lists the new backend and whose protocol crosses the shape in each applicable direction (§ 2). For most shapes the existing `ffi_` / `exec_` goldens already list every backend, so adding a backend means adding it to those `build { ... }` blocks and making the emitter pass — not writing new goldens. A shape with **no** existing runtime golden on any backend is a corpus gap the new backend surfaces; write the golden (it benefits every backend).
- Backend-first `HOST_INTERFACE` / `ARTIFACT_SHAPE` cases remain separate acceptance evidence. They cannot compensate for any red, build-only, or absent runtime cell.
- If the new host genuinely cannot express a shape, stop and surface that caveat to the user before landing. A correctly bannered and discharged impossibility remains an `I` cell; normal acceptance does not turn it green.
- The new backend's column has **no** build-only, absent, stubbed, or `I` cells. The column must be fully green (modulo § 5.7 not-mandated coverage gaps, which are corpus-wide, not backend-specific).
- The new backend has a green polymorphic application-stage runtime witness
  for all four § 4 shapes; representation erasure has not flattened its
  application tree.
- The new backend has a green recursive control-flow stack-safety witness;
  finite tail `Continue` execution does not accumulate host wrappers or host
  frames at the causally established fixed depth, while same-ABI real
  conversion, ABI changes, and pending continuation work remain intact.

`/add-backend` invokes the audit, reads the matrix, and blocks on the new column unless every applicable cell is green. "The backend builds the corpus" is necessary but not sufficient — the audit additionally proves every shape *crosses at runtime*, which a build-only pass does not. The gate is what makes "a build-only / stubbed shape can never hide again" structural rather than a matter of remembering to check.

## How to report

Produce the **matrix** as the headline artifact: rows = shape axes (§ 1), columns = `(backend × direction)` (§ 2), each cell one of:

- `R{golden:protocol}` — runtime-exercised, naming the golden and protocol that covers it.
- `I{spec-banner / docs-banner / spec§ / emit§}` — a genuine impossibility recorded as a concrete known caveat, naming both banners and cite sites with the discharge verdict. `I` is not green.
- `—` — not applicable (shape has no meaningful crossing in that direction; state why).
- **`RED`** — build-only / absent / stubbed with no carve-out. The finding.

Then group findings:

1. **Red cells — missing runtime coverage** — `(shape, backend, direction)`, why it's red (build-only / absent / stubbed), and the focused golden to write (or `run.args` protocol to add). Each is a missing-golden bug per AGENTS.md § Universal rules.
2. **Per-backend opt-outs** — runtime goldens whose `build { ... }` omits a backend other goldens cover (cross-ref `audit-corpus`).
3. **Caveated cells** — each `I` cell, its mirrored banners, and why the backend column is not fully green.
4. **False-impossibility carve-outs** — claimed-impossible cells whose discharge verdict is `fixable` against the escape-hatch standard (cross-ref `audit-spec-drift` § 7).
5. **Internal-vs-boundary conflations** — any HKT golden credited with boundary coverage it doesn't have (its host fns don't cross a dictionary), or a `(deferred)`-style comment manufacturing a false alarm about an internal-only carrier.
6. **Spec-admitted-not-mandated coverage gaps** — red cells for shapes the spec admits but does not mandate at that cell (the *category* the HKT-boundary dictionary cell illustrates, though that cell is itself covered — `ffi_host_functor_dict_roundtrip`). Reported as coverage gaps to fill, distinct from conformance findings.
7. **Polymorphic application-stage witness** — per backend, cite the running
   adjacent-direct-binder, singular returned/computed-application,
   adjacent-first-class-binder, and observable-order evidence plus the
   occurrence-appropriate emitted realization. Missing or flattened stages are
   findings even when the boundary matrix is green.
8. **Emission substitution** — any matrix cell credited to an emission case, or any claim that an independently usable facade closes a missing runtime crossing. Cite the supplemental emission evidence separately without changing the cell.
9. **Recursive stack-safety witness** — per backend, cite the deep finite
   structured-state GREEN result with its exact iteration count and expected
   result. Per semantically distinct adaptation owner or realization, cite the
   executing wrapper-growth RED, restored GREEN, and causal exact-carried-value
   no-wrapper guard; one shared-layer pair covers its consumers, while each
   backend-local owner requires its own pair. Cite same-ABI real-conversion,
   different-ABI, and genuine-pending-work controls. Proportional host-stack
   growth, a compile/setup-only RED, or missing evidence is a finding even when
   ordinary recursive cases pass.

For each finding, cite the shape's spec anchor (§ 1), the golden/protocol (or its absence), and the backend's family.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md): for a red cell whose shape already has a runtime golden on other backends, add the missing backend to the golden's `build { ... }` block and run it; for an absent shape, write the focused roundtrip/exec golden. Do **not** autonomously land an emitter change to make a newly-written golden pass — that is backend-emitter work (`kio-rs/src/backends/`) gated by [`ai/topics/emit.md`](../../topics/emit.md); surface the emitter gap as the finding and let it be fixed deliberately.

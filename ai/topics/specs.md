# Specifications

Trigger: editing under `specs/`, or adding a new spec page.

The `specs/` directory holds the authoritative specifications. These are **contracts**, not prose documentation — each file describes settled behavior and should be kept strictly consistent with the implementation. Pedagogical and reader-facing material lives in `docs/`; see [`ai/topics/docs.md`](docs.md).

**A spec never carries a "future extension" note** — no deferred features, no "not yet supported", no sketches of unbuilt behavior; direction lives in `ROADMAP.md` and nowhere else. State a deliberate omission as present-tense contract ("there is no X; Y is the supported route"), with its error category when the omission is a rejection. Reservations (a spelling or range rejected today to keep design space open), concrete current-caveat banners, and the exact per-backend Host API stability field are the admissible exceptions; backend maturity and ad hoc stability banners are not. The full class catalogue is in [`no-future-extensions.md`](no-future-extensions.md).

- **[`specs/prime.md`](../../specs/prime.md)** — **Kio'**, the small desugared subset that carries the formal semantics and is the self-hosting target. It is a strict subset of `language.md`: every Kio' program is a Kio program with the same meaning, and there are no Kio'-only productions, identifier classes, or semantic privileges.
- **[`specs/language.md`](../../specs/language.md)** — the **full** Kio surface: everything in core plus syntactic sugar (tuple literals, multi-label construction, `labels`, `alias`, `.<marker>` placeholder lambdas, `op` user operators, UFCS), imported block elaborator calls (`if!`, `scope!`, `do!`, `match!`), and blockless elaborator calls (`iso!`, `into!`, `onto!`, `align!`, `ease!`, `atom!`, the spine palette, `derive!`; `ease!` additionally serves as the variance-directed function-type adapter for `->`-typed source/target). Every surface feature notes how it relates to core.
- **[`specs/grammar.md`](../../specs/grammar.md)** — the canonical grammar productions for Kio and Kio', plus the package-file grammars. Organized in three layers (Kio' / Kio surface extensions / package files); `language.md` and `prime.md` cross-link into it for productions while keeping the prose narrative themselves.
- **[`specs/package.md`](../../specs/package.md)** — the shape of a Kio package (package file, host declarations, the bridge block, build block).
- **[`specs/cli.md`](../../specs/cli.md)** — the `kio` command-line program.
- **[`specs/style.md`](../../specs/style.md)** — the canonical style produced by `kio fmt` (indent, A1 leading-comma layout, `import`-block ordering, literal canonicalization).
- **[`specs/exit-codes.md`](../../specs/exit-codes.md)** — the Unix exit-code convention: each error category (parse, type, elaborator, …) maps to a distinct exit code so tests can assert error kind without depending on implementation-specific message text.
- **[`specs/diagnostics.md`](../../specs/diagnostics.md)** — the structured content of a compile-time diagnostic (primary span + message, secondary labels, `help`, `note`, `suggestion`) and its caret/snippet rendered layout. Splits the cross-implementation contract (structured content) from incidental rendered text, the companion to `exit-codes.md`'s category contract.
- **[`specs/kiodoc.md`](../../specs/kiodoc.md)** — the **Kiodoc** directive contract: the fence-attribute vocabulary (`harness=NAME`, `placeholder=...`, `file`, `accumulate`, `@NAME`, `ignore`, `variant=KIND`, `check_exit_code=N`, `stdout`, `stderr`, `run_exit_code=N`) and HTML-comment hidden-fence form that let Kio source blocks inside Markdown files be validated by `kio doc` and any other consumer of the same contract.
- **[`specs/backends/`](../../specs/backends/)** — per-backend contracts for the shipping emitters (JS, TypeScript, Python, Java, Rust, Go, Swift, Haskell): output layout, host record shape, package API surface, anything a host needs to call into a kio-built artifact. Shipping backends are presumed to implement the full Kio contract and carry no independent backend version or maturity tier. Each carries the required Host API stability field, which governs compatibility only. A concrete known caveat, if one exists, is stated in a top banner mirrored by the corresponding `docs/hosts/<lang>.md` guide. Cross-backend properties that apply universally or under a semantic capability predicate live in [`specs/backends/README.md`](../../specs/backends/README.md). Today: [`specs/backends/README.md`](../../specs/backends/README.md), [`specs/backends/js.md`](../../specs/backends/js.md), [`specs/backends/ts.md`](../../specs/backends/ts.md), [`specs/backends/python.md`](../../specs/backends/python.md), [`specs/backends/java.md`](../../specs/backends/java.md), [`specs/backends/rust.md`](../../specs/backends/rust.md), [`specs/backends/go.md`](../../specs/backends/go.md), [`specs/backends/swift.md`](../../specs/backends/swift.md), [`specs/backends/haskell.md`](../../specs/backends/haskell.md).
- **[`specs/formal/`](../../specs/formal/)** — formal companions to the prose specs. Pins the meta-theory (typing rules in inference-rule notation, reduction relations, type safety / strong normalization / decidability statements with proof sketches) into a written-down argument. Today: [`specs/formal/README.md`](../../specs/formal/README.md), [`specs/formal/prime.md`](../../specs/formal/prime.md), [`specs/formal/elaboration.md`](../../specs/formal/elaboration.md), [`specs/formal/equiv.md`](../../specs/formal/equiv.md). The prose pages are the contract; the formal pages are the meta-theory companions kept strictly in step. The reference coercion-elaborator libraries' per-form semantics are not language meta-theory — they live as case studies of the executable POC in [`docs/poc/`](../../docs/poc/).

[`README.md`](../../README.md) links to [`specs/`](../../specs/) as a whole — one "authoritative contracts" directory pointer for public-facing readers, not a curated per-page subset. The exhaustive per-page list lives here; the README's on-ramp is the directory link. Because the README names no individual page, a spec rename or removal needs no mirrored README edit — only moving or renaming the `specs/` directory itself does.

The `kio` CLI documented in [`cli.md`](../../specs/cli.md) is the *public* command-line surface. The `kio-prime` binary built from the same crate is an **internal** tool: a Kio'-only compiler used as a reference and to gate test goldens that claim Kio'-ness. It is intentionally not specified in `cli.md` and should not be added there.

**Keep every file in `specs/` up to date alongside any change that affects its subject.** The authority baseline is the specification at the start of the user-requested unit of work, before any agent-authored spec or implementation edit for that work. The effective authorized contract adds only specific user instructions or approvals given before implementation that deliberately revise or settle that baseline; otherwise, stop before implementing a contradiction. A normative spec edit needs that prior authority and cannot bootstrap it, even when the spec and implementation are split across commits, branches, sessions, or delegated tasks. Current implementation behavior, a new test, and an audit or review finding are evidence rather than authority, and a spec change whose authority is disputed cannot authorize follow-on work. After-the-fact approval starts a newly authorized work unit; it does not erase the earlier violation or waive a fresh review of the implementation and every required paired artifact. Editorial corrections and clarifications that preserve every normative behavior are not contract changes; state that limited effect when it could be mistaken for one.

## What goes into a backend spec

Each `specs/backends/<lang>.md` is a contract for a single backend — how a host invokes a kio-built artifact in that language. The audience is a host author, not someone modifying the codegen. The page should contain, and *only* contain, what a host needs to call into the artifact:

1. **Family, API stability, and caveats.** Put exactly one `Host API stability: evolving|stable` field immediately below the title, formatted like the backend pages, follow it with an adjacent link to [`specs/backends/README.md` § Host API stability](../../specs/backends/README.md#host-api-stability), and mirror the field and link in `docs/hosts/<lang>.md`; the only admitted values are `evolving` and `stable`. A new backend starts `evolving`. Renaming a backend page and guide is removal of the old ID plus addition of the new ID: status does not transfer, a stable old ID is demoted in an earlier publication, and the new ID starts `evolving`. Promotion and demotion both require explicit user approval, and each transition commit records the reviewed change with the footer trailer `Host-API-Stability-Transition: <backend> <old> -> <new>`. A promotion establishes the then-documented API as a non-retroactive compatibility floor; a demotion is published before any break, and the break still needs its own explicit approval because `evolving` is not standing authority. The field concerns compatibility only and never relaxes correctness, completeness, conformance, or backend acceptance. Kio' is excluded. Declare the page's **language family** per [`specs/backends/README.md`](../../specs/backends/README.md) § Language families (`Family: <name>` where `<name>` is one of the families listed there). Do not assign the backend its own version, maturity tier, or any other stability label. The normal page has no banner. If a shipping backend has a concrete known caveat, put a specific top banner here and mirror it in `docs/hosts/<lang>.md`; “experimental” and “may evolve” are not caveats. A caveat banner reports the current defect or host-source incompatibility and never converts fixable work into an accepted limitation. [`ci/checks/repo-lint/backend-api-stability.sh`](../../ci/checks/repo-lint/backend-api-stability.sh) enforces field shape, allowed values, backend/guide coverage, mirror parity, the `evolving` introduction default, rename-as-remove/add semantics, every backend-page addition/removal and merge-parent edge, and exact footer trailers. A merge may inherit an already-validated parent state; a weakening or removal cannot inherit from a branch that predates the stable state. Only the commit structurally identified by the gate's first addition may retrofit fields onto existing pages; its parents' ancestry is the pre-policy baseline, so side history first merged later is checked even when it has no surviving tree delta. Later backends carry the field in their addition commit. Its history checks require a complete clone; the Linux CI checkout fetches full history for this gate.
2. **Language version.** State the concrete ecosystem-native host-language or
   runtime floor the emitted artifact requires (an edition, standard revision,
   compiler series, or runtime line), name every stable gating feature that
   pins it, and name every ordinary stable source pragma, manifest/edition
   declaration, or language mode required to use those features. This is the
   frozen public compatibility floor selected at backend admission or an
   explicitly authorized re-baseline under
   [`ai/topics/emit.md`](emit.md) § Stable host release and capability floor;
   it is neither a floating "latest" claim nor the repository's exact
   development/CI toolchain resolution. Ordinary stable source- and
   build-carried language modes are admissible; preview, experimental,
   release-candidate, nightly-only, unstable feature-gated, and
   experiment-only opt-in capabilities such as `GOEXPERIMENT` are not. Do not
   base the contract on a capability missing from any runtime/toolchain class
   the page admits. A later floor increase is a host-compatibility change and
   updates the spec, guide, migration analysis, and floor-enforced evidence
   together.
3. **Output layout.** What files `kio build <id>` writes under the target's `out` directory: which is the entry point, which are internal supporting blobs. Internal files are described by role, not by internal shape.
4. **Loading protocol.** The steps a host performs to bring the package online — install host bindings, evaluate / link / load emitted artifacts, etc.
5. **Package API.** What's reachable on the value the host receives after loading: the `pub` items the bridged modules expose (under their module namespace), and the generic intrinsic API the package exposes for constructing and inspecting opaque structural values. Document the methods, not the underlying value shapes.
6. **Host record contract.** How current `host fn` declarations map to
   host-supplied callables. Current-host completeness includes live
   requirements only — no implicit per-role operations, ambient runtime, or
   retained-history obligation. Spell this out so users don't read a per-role
   value-layout or compatibility table as a checklist of items they have to
   provide. When sealed signature history affects the facade, the page also
   carries a **Deprecated host items** section naming the target's deprecation
   spelling, how a new host omits both removed function implementations and
   removed type selections, the shim's absence from loader/runtime/package
   dispatch, and the concrete caveat when optional retention is impossible.
   The page and matching host-guide top banner also name the exact source edit
   when incompatible declaration epochs force the shared planner to omit a
   retained root or type closure.
7. **FFI surface.** How values cross the FFI boundary, in three categories:
   - **Atomic types** (`role`-tagged host types): the JS-native shape per role. Hosts construct and consume these directly.
   - **Structural and nominal types** (built from `&`, `|`, `newtype`, `labels`): opaque package values, constructed and inspected via the package API. The page documents the API methods, not the underlying shape.
   - **Carve-outs** (unit, function values): these flow as JS-native (e.g. `null`, JS functions for the JS backend) for ergonomic reasons; the page lists them explicitly so hosts know how to construct them.
8. **Item naming.** Whether the backend imposes name mangling visible at the FFI surface (e.g. reserved-word collisions, illegal characters). State the rule, or state explicitly that no mangling is needed.
9. **Worked example.** A minimal end-to-end snippet exercising the above.

**Cross-backend properties live in [`specs/backends/README.md`](../../specs/backends/README.md), not on each per-backend page.** This includes the Host API stability contract, universal rules, and rules selected by a semantic capability predicate. The shared preamble enumerates the synchronous calling convention, type erasure at the FFI, open-world property, package isolation, exception propagation, well-foundedness inheritance, and behavioral additivity. A backend page carries its status field, references the applicable shared rules by section number, states its current host realization or concrete caveat, and does not redefine the shared law. New backend pages start by linking the preamble and only spell out what's backend-specific.

Choose the normative home from semantic scope, never from the backend where a
rule was discovered. A universal or capability-cohort law belongs in the
shared README; a cohort may cross family boundaries. A family section is
appropriate only after every current member's independent evidence agrees. A
per-backend page owns host syntax, output layout, integration mechanics, and
the backend's realization of shared laws.

A newly authored or substantively revised conditional shared law begins with a
stable heading followed immediately by `**Applies when:** <semantic
predicate>`. The predicate names the relevant host capability, boundary
occurrence, or artifact property; it does not name a backend or infer
applicability from family membership. Existing conditional laws remain part of
the semantic audit inventory even when they predate this marker. Before adding
or changing normative text, classify its authority under AGENTS.md § Universal
rules — Behavioral contracts cannot be rewritten retroactively.

An authorized shared law is complete only after every shipping backend and
semantically distinct occurrence has an applicability, conformance, and
evidence result under [`ai/topics/emit.md`](emit.md) § Decision record and
propagation gate. Land current normative prose with the conforming
implementations and evidence. Keep rationale and incomplete rows in the owning
engineering or tracking artifacts; an owned incremental residual does not make
the shared contract complete.

Out of scope:

- **Per-target build-block keys** beyond `out` — those live in [`specs/package.md`](../../specs/package.md) § Build target files and may grow per-target sections there over time.
- **Codegen internals** that don't cross the FFI boundary — module wrappers, internal name mangling for binder positions, optimization passes.
- **Convenience shortcuts the implementation emits** that hosts shouldn't rely on.

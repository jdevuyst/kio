---
name: audit-runner-host-fidelity
description: Verify each backend test runner constructs the host and invokes the package as a real host would, conforming to the authoritative backend spec without contradicting its host guide
allowed-tools: Read, Grep, Glob, Bash
---

# Runner host-integration fidelity audit

The per-backend test runners under [`ci/infra/kio-test-runner-rs/`](../../../ci/infra/kio-test-runner-rs/) are the executable stand-in for a real host: pointed at a `kio build` artifact, each one brings the package online and calls into it. For the corpus to mean anything, a runner must do this **the way a documented host author would** — implement the published host trait / record, instantiate through the published factory, and navigate the published package surface — not through a private constructor, an internal symbol, or an undocumented shortcut. If the runner integrates the package in a way a real host couldn't, a passing golden no longer proves the *documented* integration works.

This skill checks the runner's host-construction **technique** against the two host-facing contracts:

- **[`specs/backends/<lang>.md`](../../../specs/backends/) is authoritative.** Its § Loading the package, § Host record contract, § Package API, and § Item naming define how a host stands the package up and reaches it. A runner technique with no backing here is a finding. The audience of these pages is a host author, not a codegen maintainer — see [`ai/topics/specs.md` § What goes into a backend spec](../../topics/specs.md).
- **[`docs/hosts/<lang>.md`](../../../docs/hosts/) is the narrative companion.** It may legitimately *omit* detail — a host author can read the generated crate / module or fall through to the spec. So an omission is **never** a finding. A concrete claim it makes that **contradicts** the spec or the emitter is — per [`ai/topics/docs.md`](../../topics/docs.md) ("if `docs/` and `specs/` diverge, `docs/` is the bug").

**Not** this skill's job (covered elsewhere, cross-reference to avoid double-reporting):

- Whether the runner *reads* `kio build` output, and whether each named
  protocol remains the sole semantic authority for its complete host types,
  host functions, native fixtures and bodies, exports, and execution —
  [`audit-test-strategy`](../audit-test-strategy/SKILL.md) § Runner
  independence and § Runner-protocol parity. This skill assumes independence
  holds and asks the orthogonal question: given that complete protocol-owned
  contract, does the runner *build the host and call the package* the
  documented way?
- Whether the **emitter** behaviorally conforms to `specs/backends/<lang>.md` (spec says X, emitter does Y) — [`audit-spec-drift`](../audit-spec-drift/SKILL.md) § 7 Per-backend specs. This skill is the host-boundary complement: its § 2 below compares the mangler *transform* across runner ↔ emitter ↔ spec ↔ docs, which catches a description that is mechanically wrong yet output-equivalent on today's legal inputs (the "lower-cased" class) — a drift a behavioral spec→impl check passes over because both sides still produce `app__print`.

Anchor: [`AGENTS.md` § Pointer set](../../../AGENTS.md) →
[`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md)
(§ The protocol model and § What runners read from emitted artifacts), plus
the per-backend contracts above and the independent fixed-host contract in
[`test-data/emissions/README.md`](../../../test-data/emissions/README.md).

**Read first:** the runner README, then the shipping-backend list from [`specs/backends/README.md`](../../../specs/backends/README.md) § Backend pages — read it at run time; do not hardcode. **Every shipping backend's runner is in scope**: one runner bin per backend under [`ci/infra/kio-test-runner-rs/src/bin/`](../../../ci/infra/kio-test-runner-rs/src/bin/) plus the shared helpers in `src/shared/`, checked against that backend's `specs/backends/<lang>.md` and `docs/hosts/<lang>.md`. Apply §§ 1–7 to each backend; Rust and JS are the worked examples threaded through the sections — the Rust driver is synthesized in [`build_driver`](../../../ci/infra/kio-test-runner-rs/src/bin/kio-test-runner-rust.rs), the JS host record in [`build_host_record_expression`](../../../ci/infra/kio-test-runner-rs/src/shared/js_exec.rs). For every other backend, locate the same two sites in its runner (the driver/host-construction synthesis and the host record/impl builder) and ask the same questions.

## 1. Factory / entrypoint parity

For a current package, the runner implements, provides, and selects live host
items only. A retained-history default may remain in the generated facade, but
the runner must not override it, choose a retained type/generic binding, or
otherwise mask a history-derived obligation that a real new host would face.

The runner must bring the package online only through the **published** factory and implement the **published** host interface.

- **Rust** (`build_driver`, the `driver.rs` template it `format!`s). The generated driver must:
  - `use <crate>::host::<Handle>Host;` — the published branded trait path, its name derived from the crate name (`specs/backends/rust.md` § 2. Implement the host interface; `docs/hosts/rust.md` § 2. Implement the host trait).
  - `impl <Handle>Host for StubHost { … }` — implement that trait.
  - construct via `<crate>::create_<ident>(StubHost)` — the published branded factory (`specs/backends/rust.md` § 3. Instantiate and invoke).
  - **Finding:** the driver hardcodes a fixed handle/trait/factory name instead of deriving them from the artifact's published crate name, implements a non-`host` trait, or reaches a `crate::__kio_runtime::…` / `crate::shapes` internal to *stand the host up* (referencing `crate::shapes::…` to spell a parameter type is the published Package API and is fine — see § 5).
- **JS** (`build_host_record_expression` + the factory call site). The runner must instantiate only via the branded `create<Handle>(host)` factory — its name derived from the effective artifact namespace, not a fixed `createPackage` (`specs/backends/js.md` § 3. Instantiate and invoke) — passing a plain namespaced record (§ 2. Build the host record). Installing native callables (`__kio_host_print__`, …) on the global and referencing them from the record's arrow bodies is an embedding mechanism, **not** a package-internal reach — not a finding. **Finding:** the runner reads or mutates package-internal state (the frozen internal record, evaluated-module internals) instead of going through the factory + the returned surface.

- **Every other backend** — the same two checks against its own published contract (`specs/backends/<lang>.md` § 2. Implement the host interface / § 3. Instantiate and invoke): the runner must implement the published host construct and instantiate only through the published factory. A private constructor, an internal symbol, or an undocumented shortcut at host-setup sites is a finding.

- **Opaque host-type policy** — distinguish the facade the backend actually
  publishes:
  - For configurable or defaulted type bindings, the runner fills every
    separately named entry and may choose the same concrete host type or type
    constructor for multiple compatible Kio declarations. Exercise an
    override when defaults are part of the contract.
  - For fixed or erased mappings, there are no host-selected type entries to
    fill. Confirm that the runner follows the documented mapping; do not
    demand or synthesize configurable slots.
  In either case, treating a shared representation as a collision, or
  generating wrappers, brands, tags, defined types, `NewType`s, or markers
  solely to keep opaque host representations distinct, is a finding.

Detection: read each runner's driver template / record-builder strings and confirm the entrypoints; grep the runner bins for direct `Package` construction or `__kio_runtime` used at host-setup sites.

## 2. Host-boundary mangler parity (3-way + docs)

The single highest-value check: the name a host must spell for each `host fn` has to agree across **runner ↔ emitter ↔ spec** (and not be contradicted by docs). This is the check that catches a recurrence of the "lower-cased" drift.

- **Rust** — compare, literally, the transformation in all four:
  1. `rust_host_member(module, leaf)` — [`src/shared/host_api.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/host_api.rs).
  2. `mangle_host_name` — `kio-rs/src/backends/rust/emit.rs`.
  3. `specs/backends/rust.md` § Host record contract ("Namespaced method names") and § 2. Implement the host interface.
  4. `docs/hosts/rust.md` § 2. Implement the host trait.
- **JS** — compare:
  1. `js_host_namespace(module)` — `host_api.rs`.
  2. `host_module_key(module)` — `kio-rs/src/backends/public_names.rs` and
     its host-call / host-record use sites in
     `kio-rs/src/backends/js/emit.rs`. The similarly named
     `module_namespace` / `module_namespace_from_key` functions identify
     internal emitted modules and are not the public host-record transform.
  3. `specs/backends/js.md` § 2. Build the host record / § Host record contract.
  4. `docs/hosts/js.md` § 2. Build the host record.

- **Every other backend** — the same 4-way ladder: the runner-side name helper in [`src/shared/host_api.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/host_api.rs), the emitter's mangler in `kio-rs/src/backends/<lang>/emit.rs`, `specs/backends/<lang>.md` § Host record contract / § Item naming, and `docs/hosts/<lang>.md`.

The same ladder applies to the **package-namespace derivation**: the runner's
independent transform from the harness-supplied source package name (the
default-rule input) or current target's effective namespace override ↔ the emitter's
`default_*_namespace` + brand derivation in
[`kio-rs/src/backends/namespace.rs`](../../../kio-rs/src/backends/namespace.rs)
↔ `specs/backends/<lang>.md` § Output layout / § Item naming ↔
`docs/hosts/<lang>.md`. Compare the transform (PascalCase split, keyword /
unimportable-name mangles), not just the worked example. Reading an emitted
manifest, source header, marker, or filename to rediscover identity is an
independence finding owned by `audit-test-strategy`.

Compare the transform itself — case handling, source `_` versus module `/`,
path boundaries, the `__` join where used, and target escaping — not just the
worked-example output. The runner protocol must retain exact `(module, leaf)`
fields until this rendering step; reconstructing ownership with
`rsplit_once("__")` or another parse of an already-flattened host name is a
finding. **A prose rule that describes a transform the code does not perform
(or omits one it does) is a finding even when the output coincides on today's
legal inputs**, because it misleads a host author and rots silently. When the
output coincides only because of a constraint, state the constraint
(module/subdirectory/package segments are lowercase per
`specs/language.md` § Naming conventions, so a Rust "lower-cased" step would
be vacuous; JS genuinely upper-cases) — and still flag any description that
claims a step the mangler doesn't take.

## 3. Package-surface navigation parity

The export paths the runner calls must match the published package-surface
namespacing.

- The runner reaches exports through the documented role-bearing facade tree.
  Root-module selectors use the backend's documented root rendering (usually
  the source spelling, with a keyword/unspellable fallback); nested-module,
  newtype-handle, and value selectors use their documented disjoint
  renderings. Confirm each runner's navigation expressions follow its
  backend's rule for every protocol's export-set (the roundtrip protocols
  navigate to bespoke exports — check each).
- **Finding:** a navigation shape the spec doesn't document (reaching a
  private item; a flattening the spec doesn't promise; treating a
  value `child`, nested module `child`, and public newtype `Child` as one
  selector; or conflating any distinct selectors whose candidate host
  renderings collide under the host language's identifier equivalence). The JS
  runner probing a value `child` and nested module `child` is legitimate only
  when both probes use the two distinct selectors documented by the backend.

## 4. Host-body realism

The host-fn bodies the runner supplies must be ordinary host code — a plain trait-method body (Rust) / plain callable on the record (JS) — exactly as a host author writes "whatever backing logic it likes" (`specs/backends/rust.md` § 2. Implement the host interface).

- Synthesized canonical bodies and protocol-specific bodies are legitimate — a host's body is its own business. **Not** a finding.
- JS I/O delegating to native globals via the embedding engine. **Not** a finding.
- **Finding:** a body that only works through a non-published coupling — reading a package-internal symbol, depending on emit order, or installing the callable under a key the package's documented lookup would not read.

## 5. Shape-naming through documented aliases

When a `host fn` signature names an emitted structural shape (the Rust roundtrips — sum returns, the `loop` step's result, roundtrip payloads), the runner must spell it through the **documented** boundary-alias module — `crate::ffi::env::<member>::<slot>` / `crate::ffi::exp::<member>::<slot>` (`specs/backends/rust.md` § Output layout, `src/ffi.rs`) — the same stable, host-facing name a real consumer uses.

- Naming a shape through its `ffi` alias, or rewriting a `crate::shapes::…` reference to the driver crate (the published Package API), is "as a real user would" — **not** a finding (this is the spec-compatibility complement to `audit-test-strategy` § Runner independence, which forbids *reading* build output to learn the interface).
- **Finding:** a signature reconstructed by re-deriving a structural spelling rather than referencing the emitted alias, or reaching an internal (non-`ffi`, non-`shapes`) path to name a boundary type.

## 6. Namespace parity and coexistence-driver realism

The branded facade makes package-level names derived, never fixed (`specs/backends/README.md` § The package facade):

- Every package-level name a runner spells — namespace, handle, host type,
  factory, artifact file names — must derive independently from the effective
  namespace supplied or derived from harness inputs. The source package name
  supplies only the default-namespace input; it is not itself backend artifact
  identity. A
  hardcoded package-level name in a runner bin (a fixed module/package/crate
  constant, a fixed factory string) is a finding.
- The **coexist driver** ([`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md) § The coexist protocol) must host both packages the way a real host would: each instance created through its own published factory, each behind its own host, calls genuinely interleaved (first → second → first), no cross-instance state. This is the executable check of `specs/backends/README.md` § 4 Package isolation and § The package facade § Coexistence. A coexist arm that fakes the interleave — one instance reused, cached output replayed, the second package built but never instantiated — is a finding.

## 7. docs/hosts ⊆ {spec, runner} — contradiction, not omission

Walk every **concrete** integration claim in `docs/hosts/<lang>.md` — factory name, trait/record path, mangling rule, navigation shape, host-record nesting — and confirm each agrees with `specs/backends/<lang>.md` and the runner.

- A detail `docs/hosts/` *omits* is fine; the guide is a companion and may send the reader to the generated artifact or the spec.
- A claim `docs/hosts/` makes that *contradicts* the spec or the emitter is a finding. (The "lower-cased" mangling claim was exactly this class.)

## 8. Cross-check against independently authored emission hosts when needed

The fixed named runner protocol is the default public-interface oracle. When a
backend-specific public-host fact cannot naturally and independently be
expressed by that runner, compare the necessary
`test-data/emissions/<backend>/*/HOST_INTERFACE` host with the runner's
technique. The two are intentionally independent:

- the emission host is determined independently from the public backend spec:
  normally fixed checked-in source, or for mechanically-wide repetition a
  deterministic expansion of fixed checked-in parameters; neither form reads,
  copies, greps, patches, or parses generated files to discover how to call them;
- the runner synthesizes its host solely from a fixed named protocol, receives
  an output directory opaquely from a golden, and may compile/load the artifact
  in the ordinary documented way; and
- an `ARTIFACT_SHAPE` emission is irrelevant to host fidelity except when its
  durable spec fact is the public path or manifest the documented host must use.

A `HOST_INTERFACE` emission that merely repeats a contract already proved by a
fixed runner is a placement finding, not required redundancy.

The factory, host-record/trait, namespace derivation, selector navigation, and
public type spellings used by both should agree with the backend spec without
sharing producer-derived metadata. Agreement proves two distinct things: the
protocol runner exercises cross-implementation runtime behavior, while the
fixed host proves an ordinary host author can consume the public facade. The
emission is supplemental and never satisfies a runtime backend-completeness
cell. A runner that passes only because it uses a private shortcut, or a fixed
host that was adapted from generated output, is a finding.

## How to report

Group findings into:

1. **Entrypoint / host-binding divergence** — the runner stands the host up
   via a non-published constructor or internal reach, wrongly requires
   distinct concrete representations for opaque host types, or invents
   configurable entries for a fixed/erased facade (§ 1).
2. **Mangler / namespace-derivation divergence** — runner / emitter / spec / docs disagree on a host-boundary name or the namespace-and-brand transform, or a prose rule describes a transform the code doesn't perform (§ 2).
3. **Navigation divergence** — the runner calls an export path the spec doesn't document (§ 3).
4. **Body / shape-naming coupling** — a host body or signature that only works via a non-published coupling (§ 4, § 5).
5. **Namespace / coexistence divergence** — a hardcoded package-level name in a runner bin, or a coexist arm that doesn't genuinely host two live instances (§ 6).
6. **docs/hosts contradiction** — a concrete `docs/hosts/` claim that contradicts the spec or emitter; omissions are not findings (§ 7).
7. **Fixed-host mismatch** — runner and independent `HOST_INTERFACE` evidence
   disagree on the public integration technique, or the emission host derives
   its interface from current output instead of the public spec (§ 8).

For each finding, cite the runner `file:line`, the emitter mangler / contract it should match, and the `specs/backends/` (and, where relevant, `docs/hosts/`) section.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

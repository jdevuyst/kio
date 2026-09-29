---
name: add-backend
description: Add a new host-language backend end to end — semantic-capability and family classification, emitter, independent runner, genuine runtime/public-interface and recursive stack-safety coverage, distinct emission evidence when needed, devcontainer + CI — gated on completeness/spec-drift-§7/partial-impl-§6, binder hygiene, and adversarial review
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Skill, Agent
---

# Add a backend

A **backend** is the compiler-side unit serving one host language: an emitter
that lowers Kio's post-typecheck Kio' IR into idiomatic source for that
language, plus the contract a host calls against (`specs/backends/<lang>.md`),
the host-author guide (`docs/hosts/<lang>.md`), a test runner, and the corpus
coverage that proves every spec-admitted value shape crosses that host's FFI
boundary at runtime, plus independent backend-first evidence that an ordinary
host can consume the generated public facade. The current backend set is the set of
`specs/backends/*.md` files excluding `specs/backends/README.md`; do not
hardcode a backend count in this skill or in backend work. The existing
backends are the ground truth this skill is distilled from, but the process
below is **general**.

This skill is a **checklist-driven process**, not a tutorial. It references
the canonical homes for every contract rather than restating them — the
emitter framework map in [`ai/topics/emit.md`](../../topics/emit.md), the
cross-cutting FFI contract in
[`specs/backends/README.md`](../../../specs/backends/README.md), the
per-backend page structure in [`ai/topics/specs.md`](../../topics/specs.md) §
What goes into a backend spec, the runner protocol catalogue in
[`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md),
and the framework convention in
[`kio-rs/src/backends/mod.rs`](../../../kio-rs/src/backends/mod.rs)'s module
docstring. Read those; do not duplicate them here.

**The acceptance contract is a single hard gate.** A backend is not done when
"the corpus builds." It is done when
[`audit-backend-completeness`](../audit-backend-completeness/SKILL.md)'s
`(shape × direction)` matrix for the new backend's column is **fully green** —
every applicable cell runtime-exercised by a golden, with no known caveat —
and its separate emission evidence and the rest of the backend audit set are clean,
followed by an adversarial review. § The completeness gate and § The
audit-set gate make this structural.

## Before the first emitter line: read the rules that bite

Backend work is governed by `AGENTS.md` § Universal rules and the audit
suite. Read these before touching an emitter — each one has a sweep that will
fail the change if violated:

- **Per-backend limitations: only the genuinely impossible, mutual-cited.**
  A new backend is presumed to implement all of Kio, carries no independent
  version or maturity tier, and starts with the required mirrored `Host API
  stability: evolving` field. That field concerns compatibility only and
  grants no caveat, completeness, correctness, or acceptance relief. Any known
  caveat is a hard pre-landing blocker: stop and surface it to the user; normal
  acceptance does not admit a caveated backend. A carve-out is admissible
  *only* when the host language fundamentally cannot express the spec — never
  as a way to defer fixable work. The standard is
  [`ai/topics/emit.md`](../../topics/emit.md) § The escape hatch: genuinely
  impossible only when the host has **no** escape hatch at all (no `unsafe`, no
  FFI, no unchecked cast — e.g. Elm).
  Everywhere else, "the safe path is hard" is not "impossible." If the user
  explicitly authorizes an exceptional caveated backend after the skill stops,
  the concrete caveat appears in matching top banners in
  `specs/backends/<lang>.md` and `docs/hosts/<lang>.md`, and the emitter code
  comment names the detailed spec section. That exceptional publication is
  outside normal `add-backend` acceptance.
- **Bugs surface; goldens demonstrate behavior, not workarounds.** When the
  new backend can't build / run a golden the spec admits, fix the emitter —
  do **not** omit `target=<lang>` from the golden's `build { ... }` block,
  do **not** add a `|| exit 0`-style guard, do **not** reshape the `.kio`
  source (extra annotations, monomorphisation, alias scaffolding) to dodge
  an emitter gap. Failing goldens stay in place.
  Goldens are cross-implementation language, diagnostic, and runtime tests:
  case-owned code never reads, copies, greps, patches, imports, or native-
  compiles generated host-backend files. It may pass an output directory
  opaquely to a fixed runner protocol. Kio' is the explicit exception because
  it is a specified backend-neutral phase artifact: a golden may read or
  assemble it when that artifact or dynamic-load boundary is the subject, and
  harness-owned phase checks remain valid.
- **No partial implementations.** A guard that narrows on a property of a
  spec-admitted shape and silently identity-passes the complement is a
  finding, not a limitation
  ([`audit-partial-implementations`](../audit-partial-implementations/SKILL.md)
  § 6).
- **Generated bindings are outside the user-reachable namespace.** This covers
  package/module helpers, locals and parameters, backend type parameters,
  generated aliases or nominal owners, and imports or import aliases. A
  private implementation binding reserves an unreachable identity class where
  host syntax or identifier mapping permits one; otherwise it allocates
  deterministically from the complete namespace-specific seed. A host-visible
  facade identity instead uses the stable injective semantic mapping and never
  occupancy-dependent renaming. Classify each import-introduced identity into
  the host value, type, module, package, or other namespace that actually
  resolves it; there is no presumed standalone import namespace. Keep actual
  namespaces separate unless the host merges them, and render every declaration
  and use from the same identity. A counter, hash, or conventional prefix alone
  is not a collision proof. The canonical rule is
  [`ai/topics/implementation.md`](../../topics/implementation.md) § Generated
  binding hygiene; [`audit-generated-binder-hygiene`](../audit-generated-binder-hygiene/SKILL.md)
  is a backend acceptance gate.
- **The public FFI is a human-facing API.** A backend is not acceptable merely
  because its generated facade is deterministic, collision-free, and accepted
  by the host compiler. A normal host author must be able to discover, name,
  construct, pass, and inspect boundary values with the language's ordinary
  tools and idioms, using the backend spec and host guide rather than reading
  generated internals. Preserve Kio package/module/declaration ownership in the
  host's natural namespace and type mechanisms; do not flatten unrelated
  nominal declarations into one global bucket, make opaque codec/hash names
  the normal public spelling, expose body-erasure carriers and casts through a
  statically typed facade, or document a helper that conceals an unusable real
  API. Read [`ai/topics/emit.md`](../../topics/emit.md) § Runtime model and
  discharge
  [`audit-backend-family-conformance`](../audit-backend-family-conformance/SKILL.md)
  § Human-usable host facade with representative host-authored call sites.
- **A settled backend decision propagates to every semantic peer.** State one
  backend-neutral law and its applicability predicate, then record independent
  applicability, conformance, and evidence results for every shipping backend
  and relevant occurrence. The discovery language and its family do not define
  scope. Follow [`ai/topics/emit.md`](../../topics/emit.md) § Decision record
  and propagation gate; an originating emitter being green is not completion.
- **Place rules and code at the narrowest truthful shared level.** Universal
  and capability-cohort policy is shared, including when a cohort crosses
  family boundaries. A family abstraction is justified only after every member
  independently agrees. Keep host syntax and genuinely unique integration
  leaves in the backend, and apply the same classification when an audit or
  ergonomic improvement first exposes the rule.
- **Surface forms must not survive into Kio'** — an emitter consumes strict
  Kio' already classified into `Expr::Low*` / `Expr::Enriched*` nodes; it
  never sees, and must never reintroduce, a surface-only form. (This is a
  property of the input, not something the new emitter can break, but know
  it so you don't re-derive call-site kinds the recovery passes already
  established.)
- **Checked-in files are written for outside readers.** No worktree paths,
  no session/box specs, no user identity in `specs/`, `docs/`, source,
  comments, commit messages, or CI config
  ([`ai/topics/no-leak.md`](../../topics/no-leak.md)).
- **Optimizations are justified, not assumed.** Any "for speed" complexity
  in the emitter or runner carries a recorded measurement or golden-pinned
  observable change (§ The runner names the standing example: no premature
  `-j`).
- **Performance is part of backend acceptance.** Before calling a backend
  complete, inspect planning and emission complexity, duplicated preparation
  across artifacts/targets, generated-source size and nesting, and real host
  compiler wall time and peak RSS on a natural wide package. Pin a portable
  structural firing/no-op proxy for any load-bearing bounded realization. This
  is a focused proportionality gate, not an automatic broad benchmark or an
  invitation to add unmeasured optimization machinery.
- **Finite Kio recursion must not consume host stack per iteration.** Exercise
  a deep `rec(loop)` program whose recursive state carries a structured result
  and a continuation/function value through an elaborated branch. A tail
  `Continue` that reinserts the exact carried function value already in the
  destination slot representation, with no pending semantic conversion, must
  not add another host wrapper or stack frame. Require controls that retain a
  same-ABI edge with real conversion work, a different-ABI edge, and genuine
  pending continuation work. The fixed-depth runtime witness runs GREEN on
  every backend; record its exact program, iteration count, and expected
  result. For each semantically distinct adaptation owner or realization,
  observe that same input reaches execution and fails from wrapper/host-frame
  growth with the wrapper-growing behavior restored, then passes after
  restoring the fix. One shared-layer RED/restored-GREEN pair covers all
  consumers of that layer; each backend-local owner requires its own pair.
  Retain a causal guard at the layer owning the adaptation.

The trigger table also points at
[`ai/topics/emit.md`](../../topics/emit.md) (editing under
`kio-rs/src/backends/`), [`ai/topics/specs.md`](../../topics/specs.md)
(editing under `specs/`), [`ai/topics/docs.md`](../../topics/docs.md)
(editing under `docs/`), and
[`ai/topics/kio-authoring.md`](../../topics/kio-authoring.md) (editing a
`.kio` file — the goldens). Read each before you touch that area.

## Step 1 — Classify host capabilities and the authorized family

Establish host capabilities before choosing a mechanism or treating a family
label as an explanation. Record three independent classifications; none may be
derived from another merely because existing backends often correlate them:

Apply [`ai/topics/emit.md`](../../topics/emit.md) § Stable host release and
capability floor before selecting a mechanism. This workflow is a backend
admission, so the stable-release gate and floor evidence are mandatory; apply
the same gate to an existing backend only when the user has explicitly
authorized that re-baseline. Do not let compatibility with an older release
degrade the public facade or generated-code shape that the selected stable
line can provide.

1. **Authorized language family** — the current membership and shared idioms
   stated by
   [`specs/backends/README.md`](../../../specs/backends/README.md) § Language
   families. This is contract authority for the label, not causal evidence for
   a body representation or public type relationship.
2. **Body and storage profile, per internal region** — identify which regions
   retain exact host types, which use a universal representation, and which
   require reconstruction. `ReconProfile` follows the actual region that
   consumes reconstruction; it does not follow family membership or a global
   HKT label. One backend may legitimately classify different body, storage,
   and facade regions differently.
3. **Public HKT and other type relationships, per occurrence** — for every
   semantically distinct boundary occurrence, determine the exact relationship
   the host type system can express and the complete workflow that proves it.
   Do not collapse these rows into one backend-wide binary strategy. Summarize
   a capability cohort or family only after every applicable occurrence and
   member independently agrees.

Independently of those classifications, preserve every semantic
binder/application boundary required by the current applicable contract. Check
direct, escaped, and computed occurrences against their own evidence. Never
infer flattening, hidden-call staging, or another realization from family,
body erasure, declaration provenance, or an ABI shortcut.

**Write down the classification before any code** in the decision record:
family, each body/storage region, and each public type-relationship occurrence,
with the canonical applicability, conformance, and evidence results. Cite the
shared and per-backend specs for current public behavior and the inspected
implementation for its realization. This skill does not establish a new
public carrier or adapter; an unresolved contract row requires user authority
rather than an inferred mechanism.

Record Host API stability independently of those semantic classifications. A
new backend's value is `evolving`; promotion is a separate explicitly approved
transition after the then-current documented host API exists. `Evolving` never
authorizes a compatibility break during admission.

The same record names the selected stable line, the concrete ecosystem-native
public floor, each gating capability and admitted runtime/toolchain class, and
every ordinary stable source pragma, manifest/edition declaration, or
language-mode setting the emitted artifact or host build carries to enable
those capabilities. It names the exact repository resolution (including any
explicitly pinned delegated sub-toolchain) and the official release/support
evidence used at selection time. It also records the exact-resolution corpus
run and the floor-toolchain or floor-mode compilation of both the emitted
artifact and complete host-fence set with those exact stable settings. Floor
freezing, later compatibility changes, and reason-driven toolchain updates
follow the canonical emit rule rather than being redefined here.

If the host needs a **new family row**, that is a
[`specs/backends/README.md`](../../../specs/backends/README.md) edit (the
conformance audits read the table at run time, so adding a family is a
README edit, not an audit-code change). A per-backend page that departs from
its declared family's shared idioms carries an explicit mutual-cited
divergence in host realization while still implementing the full Kio contract.
A family divergence cannot excuse an unsupported or degraded feature or serve
as a per-backend limitation.

## Step 2 — The body model and the FFI skin

These are the two zones every emitter splits into
([`ai/topics/emit.md`](../../topics/emit.md) § Runtime model). Know the line
between them before writing either: the **internal body** is the computation
no host ever reads, calls into the middle of, or sees the types of —
erasure applies here and nowhere else; the **FFI skin** is the host's typed
contract (host record, structural boundary representations, exported
signatures, the per-signature boundary wrappers) and stays fully typed and
idiomatic.

**The body** is built per the Step-1 strategy. The shared, backend-agnostic
prep already happened upstream — an emitter consumes `Module<Routed>`,
classified into `Expr::Low*` / `Expr::Enriched*` nodes by
`structural_recovery` → `recover_to_low` → `capabilities`
([`ai/topics/emit.md`](../../topics/emit.md) § Where emission sits). Dispatch
over the pre-classified families; don't re-walk for shape.

`FnDef` / `FnExpr` canonical `Signature` groups are the sole abstraction
authority: each type binder is one nested `Forall`, each value group is one
product-domain `Function`, and group order is preserved. Routed elimination
comes only from direct-call `type_args` and nested `LowTypeApplication` nodes.
Never recover grouping from source declaration spelling, callee identity, an
ABI arity, or another producer-side side channel.

**The FFI skin** realizes the cross-cutting structural conventions in
[`specs/backends/README.md`](../../../specs/backends/README.md). These are
**language-level commitments**, uniform across backends — the per-backend page
*shows* how the host realizes each, it cannot weaken one:

- **The package facade** (§ The package facade) — every emitted symbol
  (package handle, factory, host type, structural boundary types, the
  runtime-support items) is either the per-package **namespace** itself or
  lives beneath it. The namespace is derived
  from the package name in
  [`kio-rs/src/backends/namespace.rs`](../../../kio-rs/src/backends/namespace.rs)
  (which owns each backend's default derivation, explicit-value validation,
  and the keyword / unimportable-name mangles — a new backend adds its
  `default_*_namespace` / `validate_*_namespace` pair there), overridden by
  the uniform `namespace` target key. The facade is **branded**: handle =
  PascalCase of the namespace's final segment, host contract =
  `<Handle>Host`, factory = `create_<handle>` in the host language's
  casing (§ Branded naming has the table). Every path below that facade
  preserves exact source module components and their boundaries plus selector
  role (nested module, type, or value) until target-language rendering. A flat
  host namespace that discards those distinctions, or an occupancy suffix
  whose spelling changes when an unrelated declaration is added, is
  non-conformant. The renderer must be deterministic and injective under the
  host's escaping and name-equivalence rules, while remaining reasonable for
  a human host author to navigate. String-keyed lookup, untyped
  universal-value handles, and host-side casts are non-conformant wherever
  the host language has static types.
- **Host record / host-trait descriptor** (§ 8) — one item per `host fn`,
  one declaration-keyed descriptor entry per `host type`,
  **module-qualified, never flattened to leaf names** (a `host fn print` in
  module `a` and one in module `b` are distinct entries; render as a
  structural hierarchy or a mangled-injective flat surface, never a
  naive-flat collision). Where the facade exposes host type bindings, those
  entries are separately addressable but their mapping is not injective:
  compatible entries may select the same concrete host type or type
  constructor. Do not invent a wrapper, brand, tag, defined type, `NewType`,
  or marker solely to preserve opaque Kio identity in the host
  representation. Keep `(module path, leaf)` separate through protocol,
  runner, and emitter code; never recover the declaring module by splitting a
  flattened method name. Adding a backend adds a *renderer* that consumes the
  shared `host_descriptor`; it does not add a module scan of its own.
- **Products → records, sums → native/sealed** (§ Structural FFI shape
  conventions, § The right-spine walk) — the shape follows the source-level
  type walked over the **right spine** (`(A & B & C)` = `(A & (B & C))` →
  3-slot product; `((A & B) & C)` → 2-slot, first slot itself a 2-slot
  product). Sums are single-keyed n-slot. Right-associative chains flatten;
  left-associative nesting is preserved end to end.
- **Newtypes** (§ 3-step key fallback, § Higher-kinded types) — keyed by
  bare newtype name, then `<modulepath>.<F>`, then positional `_<n>`;
  `labels { f : X }`-generated newtypes key by the generated type name.
- **Roles → host types** (§ 8 Role-binding layer + the per-backend
  atomic-type table) — `host type X role(r);` admits the corresponding Kio
  syntax at `X`. Prefer host-selectable bindings; where the target supports a
  natural default, make it overridable. Use a fixed backend mapping only after
  comparing similar host languages and existing Kio backends and showing that
  a truthful, reasonably usable configurable surface is unavailable. Record
  the comparison and decision in the emitter's module documentation and the
  host-visible policy in the backend spec and host guide. A per-role default
  or fixed table is an explicit backend FFI policy, not a rule by which the
  Kio role semantically chooses or infers representation. The role never
  implies extra host functions or ambient runtime.
- **Function-type FFI canonicalization** (§ Function-type FFI
  canonicalization) — Kio's strict System F arrow right-folds multi-param
  signatures to `Function(Product(A, Product(B, C)), R)`; the boundary walks
  the right spine of `param` for **positional arity** (one host-side
  argument per spine slot; a canonical unit-domain layer has no slots, while
  substituting Unit into an already-planned slot does not erase it). **Curried
  multi-layer functions stay curried** — each `Function(Pᵢ, Rᵢ)` layer maps
  to one call. Nested products at a fn FFI slot follow the same right-spine
  arity (this is lesson territory — see § Hard-won lessons).
- **Polymorphic-newtype payloads** (§ Polymorphic newtype payloads) — a
  `newtype Monad[*F] : [A][B](F(A) & (A -> F(B))) -> F(B)` payload is
  the unary chain `Forall(A, Forall(B, Function(…)))`. Preserve its semantic
  binder/application order and keep constructor/projector identities around
  the complete payload. Determine each internal and boundary realization from
  that occurrence's current contract and evidence, not from family or body
  representation. A backend that lacks the required polymorphic-payload
  behavior is non-conformant. Fix it; if the host genuinely cannot express it,
  stop and surface the caveat rather than accepting the backend.

## Step 3 — Plug into shared infrastructure; do not re-port

The reuse story is the whole point: a new backend re-ports **nothing** of the
IR walk, the upstream recovery passes, the boundary-wrapper signature walk,
the 3-step key fallback, the right-spine decomposition, or (for typed
bodies) the body-type reconstruction walk. The framework convention is in
[`kio-rs/src/backends/mod.rs`](../../../kio-rs/src/backends/mod.rs)'s module
docstring (read it first — it defines `Profile`, `RuntimeSupport`, the
per-module `par_iter` fan-out, and the runtime-support convention). **There
is no backend trait** — registration is a convention + a dispatcher arm.

The plug-in points, in the order a new backend fills them:

1. **`kio-rs/src/backends/<lang>/mod.rs`** — the registration convention:
   `pub fn profile() -> Profile` (the declarative cost-model descriptor:
   `native_records` / `native_sums` / `native_match` / `field_access` / `gc`
   / `runtime_support`), the `lower_package(...)` entry point returning a
   `<Lang>Package` (filename→content map) or an `EmitError`, the
   `RUNTIME_SUPPORT_FILE_PATH` plus its canonical fixed-content const or
   namespace-binding content function (if `RuntimeSupport::EmbeddedFile`), and
   `pub mod emit;` (+ `skin`, `runtime`, …). Inspect `go/` and `haskell/` as
   contrasting current examples, including the latter's split `SkinProfile` /
   `ReconProfile`, but copy only the plug-in points justified by the new
   backend's independently classified regions.
2. **`kio-rs/src/backends/<lang>/emit.rs`** — the BODY: `lower_package`, a
   `BodyEmitter` (term lowering / expression emission), `role_to_<lang>_type`,
   and the exported-surface walk. Keep independently justified regions in
   dedicated files: Go separates `emit` / `facade` / `facade_skin`, Swift
   separates `emit` / `runtime` / `skin`, and Haskell separates `emit` /
   `skin` / `reconstruct`; JS and Rust keep their current monolithic banner
   sections. Follow the independently justified regions, not one backend's
   file count.
3. **The boundary facade / SKIN** — select the narrowest current planning and
   conversion architecture whose semantic predicate the new host satisfies;
   do not create a `<Lang>Shapes` registry merely because another backend has
   one.
   - Go is the prepared-facade example: collect
     `PreparedBoundaryCallableSites` once, realize semantic sites and
     site-local nominal dependencies in a language facade, and let only the
     live origin carry opaque execution capabilities into a bounded
     converter. Retained roots can contribute frozen declarations and stable
     aliases but never package execution authority; a backend's removal policy
     may render only a target-native, explicitly deprecated, genuinely optional
     compatibility declaration: structural omission where the host admits it,
     or an unconditional trapping default. It remains inert and follows live
     dominance as specified by [`ai/topics/emit.md`](../../topics/emit.md)
     § Retained host declarations. Generic product
     and sum declarations are keyed by prepared semantic identity rather than
     concrete payloads.
   - Swift is the live-only prepared-facade example: collect
     `PreparedBoundaryCallableSites` without replay, realize its exact
     semantic shells and nominal relationships once in `PreparedSwiftShapes`,
     and thread each live site's paired execution layout through the bounded
     converters. This preserves Swift's native public carriers while keeping
     the erased interpreter representation private; it does not license a
     backend-local shape registry or a second walk over raw public types.
   - The pure helpers in
     [`kio-rs/src/backends/skin.rs`](../../../kio-rs/src/backends/skin.rs),
     including the 3-step key driver and readable-name hash/claim schedule,
     are used only when the backend's specified naming rule calls for them.
     FNV-1a is the specified bounded fallback where the Rust or Java page
     calls for it; Swift uses the reversible prepared shell codec directly.
     Go's generic shell/site codecs and JS's direct keys do not hash concrete
     payload shapes.
4. **`kio-rs/src/backends/<lang>/runtime.rs`** — the runtime-support template
   or declarations, if the body needs named fragments. Go supplies an embedded
   support-file path plus a namespace-binding content function; Swift and Rust
   expose fixed embedded content; Haskell renders its private runtime
   declarations into the one-file facade. These fragments include Go/Swift's
   canonical `Unit` plus erased product/sum carrier adapters, Haskell's private
   strictness helper plus universal ADT, and Rust's `Rc<dyn Fn>` / `Rc<dyn
   Any>` ladder. Type-erased bodies often need little beyond those
   representation adapters.
5. **(regions requiring type reconstruction only) a `ReconProfile` impl** — in a dedicated
   `reconstruct.rs` (Haskell's is the ~130-line minimal template:
   `module_call_return_type` = `apply_subst(ret_ty, build_typearg_subst_from_sig(...))`,
   `accept_direct_enriched_slot` = `true`, `normalize_target_ty` = identity,
   `reconstruct_other` handles the one residual). The whole structural walk
   (`value_type_with_locals` in
   [`kio-rs/src/backends/reconstruct.rs`](../../../kio-rs/src/backends/reconstruct.rs))
   is inherited; you override only the divergence hooks. A region using a
   universal representation skips reconstruction; another region in the same
   backend may still require it.
6. **Wire it in** — `pub mod <lang>;` in
   [`kio-rs/src/backends/mod.rs`](../../../kio-rs/src/backends/mod.rs), and a
   `<lang>_backend` fn + a `match target.id.as_str()` arm in
   [`kio-rs/src/cmd/build.rs`](../../../kio-rs/src/cmd/build.rs) (validates
   target keys — including the uniform `namespace` key: duplicate check,
   the backend's `validate_*_namespace`, default via `default_*_namespace`;
   the go arm is the template — creates the output directory, writes the
   output files, including an embedded runtime-support file when the profile
   declares one). **Honor the per-module
   `par_iter` fan-out from day one** (every `lower_package_*` driver fans
   across modules, concatenated in deterministic `BTreeMap` order for
   byte-stable output).

**The generated-binding naming plan is a deliverable.** Inventory generated
values, backend type parameters, aliases and nominal owners, and identities
introduced by imports. For each family, identify every user-derived and fixed
helper identity that can share its host scope. For a private implementation
binding, choose an identity no legal Kio name can reach through the backend's
mapping, reserving an escaping/mangling class when the mapping can do so. If
neither host syntax nor a reserved mapping class is usable, record why and use
a deterministic supply seeded from the complete relevant host namespace:
value names for value bindings; type parameters, aliases, nominal owners, and
fixed runtime types for type bindings; and every authored, imported, or
generated identity visible where an import binds into that namespace,
including aliases and qualifiers when applicable. A public facade identity
instead derives from the stable injective semantic mapping. Do not conflate
host namespaces unless the host does. Compare names after the host's escaping,
raw-identifier decoding, normalization, truncation, and mangling rules. The
supply for a private implementation binding stays scoped to its lexical or
emitted owner so declarations outside that owner cannot perturb its spelling.
A colliding user binding added inside the owner may select a different private
spelling, but cannot change binding or program meaning. Public facade names do
not use occupancy-dependent suffixes: their injective semantic mapping remains
stable when unrelated declarations are added. Declaration and use rendering
consume the same identity; ad hoc per-template counters are not a substitute.
Any backend type substitution beneath `forall` alpha-freshens a colliding
binder and all bound uses before descent, including applied-head occurrences.
Property labels and structural-shape keys used only for lookup are not binding
families.

## Step 4 — The runner

The per-backend runner is what proves the emitted artifact actually runs and
crosses each shape at runtime. It constructs the host and invokes the package
**the way a real host would**
([`audit-runner-host-fidelity`](../audit-runner-host-fidelity/SKILL.md)
backstops this against `specs/backends/<lang>.md`). The protocol catalogue is
[`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md);
read it for the exact named protocols and their structured host contracts.
Golden cases pass generated directories to this runner opaquely. Independent
host-source compilation belongs to `test-data/emissions/<lang>/`, not to a
golden or an adaptive runner; the two evidence paths are both required and
answer different questions.

The runner crate is a **single shared bin-only crate**
(`ci/infra/kio-test-runner-rs/`) — adding a backend adds a *binary*, not a
new subproject, so no new version mirror is needed. The deliverables:

1. **`Cargo.toml`** — a `<lang>` feature (`["dep:tempfile","dep:blake3","dep:fs2"]`
   for a native compiler) + a `[[bin]]` with `required-features = ["<lang>"]`.
2. **`src/bin/kio-test-runner-<lang>.rs`** — `fn main` (argv parse:
   `--protocol`, `--package-name`,
   target-local `--artifact-namespace=<effective-namespace>` attached to the
   preceding `--package-name`,
   `<output-dir>`;
   `EXIT_USAGE = 2`,
   `EXIT_RUNTIME_FAILURE = 1` per `specs/exit-codes.md`) → a `<Lang>Runner`
   struct → `impl TestRunner` (the thin shared trait in
   [`src/shared/runner.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/runner.rs):
   `host_api` infallibly projects the host API from the selected named protocol
   alone and deliberately receives no artifact path — the runner reads
   **nothing** from the package or emitted interface to discover signatures,
   shapes, host-type declarations, arities, roles, or fixture representations;
   conformance is the compiler's job — and
   `execute_artifact` synthesizes a `StubHost` driver + `main`, assembles the
   build tree in memory, resolves the binary through the cache adapter, spawns
   it, captures exit). A case selects a named protocol whole. It must not
   extend that protocol through per-case semantic arguments, sidecars, emitted
   comments, or ambient state. If two cases need different typed host
   inventories or fixture choices, define distinct named protocols or use a
   genuinely compile-only route.
3. **Artifact identity and the coexist protocol.** The harness passes each
   source package name explicitly with `--package-name` and passes any
   configured namespace for the current target as
   `--artifact-namespace=<effective-namespace>` on that ordered artifact
   descriptor. Package name supplies only the backend's default namespace when
   that override is absent; it is never an artifact key. The runner
   independently applies that backend's default namespace and brand transform;
   it does not parse
   emitted source, a generated manifest, or an artifact marker to rediscover
   identity. These arguments carry build-contract identity, not semantic
   host-interface metadata. The runner also implements
   `--protocol coexist`, the one two-artifact protocol
   ([`ci/infra/kio-test-runner-rs/README.md`](../../../ci/infra/kio-test-runner-rs/README.md)
   § The coexist protocol): one host program hosting **both** packages
   through their published factories with interleaved calls. Its repeated
   package-identity arguments preserve package order.
4. **Map the structured protocol exhaustively.** Each
   [`HostFnBinding`](../../../ci/infra/kio-test-runner-rs/src/shared/protocol.rs)
   carries exact qualified identity plus a structured `HostFnBodyKind`; each
   `HostTypeBinding` carries exact identity, arity, role, and fixture. Project
   those structures directly into the backend's host signatures and bodies.
   A module or leaf controls only the emitted member spelling — never infer
   semantics from a name, suffix, signature string, generated source, or a
   global leaf table. Typed-native adapters may translate a body through the
   shared
   [`CanonicalKind`](../../../ci/infra/kio-test-runner-rs/src/shared/canonical.rs)
   rendering vocabulary, but `CanonicalKind` is not a classifier. Handle every
   protocol body and fixture explicitly; an unimplemented or unknown body
   fails loudly (`panic` / `throw` / `fatalError` / `error`), never as a
   silent no-op.
5. **A name-mangling helper in
   [`src/shared/host_api.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/host_api.rs)**
   (`<lang>_host_member`, feature-gated), plus any feature gates required by
   the shared runner modules the adapter actually uses.
6. **A one-level compile-cache adapter** under
   `src/<lang>/bin_cache/` — **mirror the `go` and `haskell` adapters**
   (`src/go/bin_cache/`, `src/haskell/bin_cache/`): a `<Lang>Cache` wrapper
   over `BuildCache`, a `<Lang>BuildTree` whose file set *both* defines the
   BLAKE3 cache key and is what `produce` writes (so keyed and compiled bytes
   coincide), a `BinArtifact` implementing **`CompilerAdapter`** (the
   one-level trait in
   [`src/shared/build_cache/mod.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/build_cache/mod.rs):
   `request` → `ArtifactRequest`, `produce` → the single compile), a flat
   `CacheError`, and a `<compiler>_identity()` probe that **bakes the host
   platform into the cache subroot**.
   - **Enroll every actual native compile in shared compiler admission.**
     Inject the shared
     [`CompilerAdmission`](../../../ci/infra/kio-ci-scheduler-rs/src/compiler_admission.rs)
     capability into the cache adapter rather than reading scheduler
     environment from cache code. Acquire with `acquire_for` only at the
     actual compiler command boundary, after the per-key lock and second cache
     probe; warm hits and same-key waiters take no permit. Cache-disabled,
     direct, and coexist compiler routes acquire at the same boundary.
     `acquire_for` also propagates the held-resource marker so a compiler that
     launches another proxied compiler does not deadlock by reacquiring. Do
     not add a backend-specific gate.
   - **Thread the shared debug compiler observer through every compile route.**
     Parse `KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER` with
     [`compiler_observer.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/compiler_observer.rs)
     independently of `RunnerCacheConfig`, retain it outside the cache-mode
     branch, and use its command constructor for the regular, cache-disabled,
     direct, and coexist routes. Build the complete nested command before
     `CompilerAdmission::acquire_for`. Only actual compiles are observed:
     compiler identity/version probes, runtime launches, warm cache hits, and
     same-key waiters stay bare. A non-rustc backend ignores
     `KIO_TEST_RUNNER_COMPILER_WRAPPER` but still uses the debug observer.
     Add a route-level argv test and keep `ci/infra/sccache.sh`'s readiness
     handling structural—exact configured-layer equality, never inference from
     the observer basename.
   - **Path-remap for cross-worktree reuse**: fold the compiler's path-remap
     flag into the key and the build (`-trimpath` for Go,
     `--remap-path-prefix` for Rust, `-file-prefix-map` for Swift). If the
     compiler has **no** remap flag (GHC), document the narrow residual: at
     the lowest opt level with no debug info the compiler embeds no build
     path and produces byte-identical binaries across worktrees, so reuse
     holds (mirror `src/haskell/bin_cache/mod.rs`'s comment).
   - **sccache rejects non-rustc compilers.** The
     `KIO_TEST_RUNNER_COMPILER_WRAPPER` (sccache) is threaded into the
     **rustc** invocation only; sccache passes `-E` and rejects `go` / `ghc`
     / `swiftc`. So a non-rustc runner ignores the wrapper, and **the
     content-addressed artifact cache is the runner's only acceleration
     layer** (a warm hit skips the compiler entirely). Reuse the shared
     [`runner_cache_env.rs`](../../../ci/infra/kio-test-runner-rs/src/shared/runner_cache_env.rs)
     for the cache env vars.
   - **Pin the toolchain's default caches — no machine-shared leaks.**
     "Mirror the go/haskell adapters" is about the cache *structure*, not
     only what those adapters happen to redirect — so enumerate every
     cache/state location the new compiler writes *by default* (build
     caches, clang-style module caches, package-manager dirs) and pin each
     per-compile under the staging tempdir (`out_dir`) or into the cache
     root. A bare compile leaves them at a machine-shared default
     (`~/.cache/go-build`, `~/.cache/clang/ModuleCache`), which the
     runner-hygiene norm forbids (runner README § Runner build cache and
     compiler wrappers) — Go pins `GOCACHE`, Swift pins
     `-module-cache-path` for exactly this reason. Don't reason from
     "does the toolchain cache have a locking race?" — the norm is
     hygiene (keep machine-shared writes inside the cache root), not
     race-avoidance. Verify mechanically:
     [`ci/checks/orchestrators/runner-cache-hermeticity.sh`](../../../ci/checks/orchestrators/runner-cache-hermeticity.sh)
     runs a tiny golden per cache-backed target with `XDG_CACHE_HOME`
     redirected to a scratch dir and fails on any file written outside the
     runner's `kio/` cache root; add a `probe_target <target> <compiler>`
     line there for the new cache-backed target in the same change.
7. **Compile profiles — selectable, cheap by default; parallelism measured,
   not assumed.** The runner compiles at a `--profile`-selected optimization
   level (also settable for a whole run via `KIO_TEST_RUNNER_PROFILE`).
   The three profile names are backend-independent — deliberately *not*
   `opt-level=N`, since the numbers differ per compiler — and each runner
   maps its selection to the compiler's real flag, with **debug info off on
   all three**:

   | profile       | rustc            | ghc   | swiftc   | intent |
   | ------------- | ---------------- | ----- | -------- | ------ |
   | `unoptimized` | `-C opt-level=0` | `-O0` | `-Onone` | fastest compile |
   | `default`     | `-C opt-level=1` | `-O0` | `-Onone` | CI default — each backend's current cheap level |
   | `optimized`   | `-C opt-level=2` | `-O2` | `-O`     | slowest, most opt-sensitive |

   `default` preserves **each backend's current cheap level**. Rust's
   `opt-level=1` is near-free over `-O0` yet still runs the optimizer, and
   that mild opt has caught a real codegen bug `-O0` would have masked — so
   Rust's `default` sits one notch above `unoptimized`. Under ghc / swiftc,
   mild optimization (`-O1` / `-O`) costs real compile time, so `default`
   stays at the zero level (`-O0` / `-Onone`, coinciding with `unoptimized`)
   and `optimized` (`-O2` / `-O`) is the on-demand thorough pass. Go has no
   optimization levels, so the profile is a no-op there (the flag is still
   accepted and validated for parity). A new backend maps the three names to
   its compiler's flags and **folds the selected profile into its artifact
   cache key** (the profile name, or the real `-O` flag it maps to) so a
   `default`-built artifact is never served for an `optimized` request.

   **Do not add `-j` / `-num-threads` / codegen-unit parallelism without a
   measured win** — at the corpus's per-file scale the compile is dominated
   by compiler startup + frontend, not codegen, so in-compile parallelism has
   nothing to parallelize. The Swift and Rust runners both measured exactly
   this (a wash on each) and ship without it; the recorded measurements live
   in the [`audit-compiler-performance`](../audit-compiler-performance/SKILL.md)
   § 8 registry — the standing instance of `AGENTS.md` § Universal rules —
   "Optimizations are justified, not assumed." The acceleration is the cache.
8. **Protocol host-body completeness gate.** Add a mechanical runner test
   (unit test or CI check) that enumerates every `RunnerProtocol` the new
   runner supports, expands that protocol's required host items, and proves
   the runner can construct a host body for every item. Do not rely on
   representative golden execution to discover this: a required host item may
   be declared by a package even when one happy-path run never calls it.
   Missing canonical / roundtrip / bespoke bodies are runner bugs, not
   optional coverage.

## Step 5 — The corpus upgrade (runner-first, emissions only when distinct)

The corpus upgrade starts with cross-implementation goldens that prove
language, diagnostic, runtime, and public-interface behavior through fixed
ordinary runner protocols. The runner's ordinary host compile/load/run is the
default public-interface oracle.

Add a backend-first `test-data/emissions/<lang>/<case>/` case only for a
backend-specific public-host fact that runner cannot naturally and
independently demonstrate, or for a durable generated-artifact fact. A
runner-duplicating emission is misplaced.

An emission never satisfies a runtime cell in
`audit-backend-completeness`.

For **most** shapes the existing `ffi_*` / `exec_*` goldens already list
every shipping backend in their `build { ... }` block. **Adding a backend
means adding `target <lang> { out
"out/<lang>/"; }` to those blocks and making the emitter pass** — not writing
new goldens. A shape with **no** existing runtime golden on any backend is a
corpus gap the new backend surfaces; write the focused golden (it benefits
every backend).

`test-data/goldens/00_success/ffi_two_packages_coexist/` is part of that
set: add the new backend's target to **both** of its packages' build blocks
(the runner-side coexist arm is the Step-4 deliverable). This is the
executable witness for
[`specs/backends/README.md`](../../../specs/backends/README.md) § The package
facade § Coexistence on the new column.

When a POC, castle, generated-package run, or other broad suite exposes a
backend bug, the fix must land with a focused golden too. Either wire an
existing focused golden so it runs on the new backend, or add a minimal
golden that reproduces the bug's shape directly. A POC/castle pass is useful
evidence, but it is not a substitute for a golden that prevents the same
backend regression from hiding in ordinary golden CI.

The backend's focused coverage also includes adversarial generated-binding
collision fixtures for every naming family it introduces in a value, type,
module, package, or other host namespace. Drive natural language/runtime paths
under nested legal Kio bindings through the fixed runner where the binding is
executable. Cover backend type parameters, generated alias/nominal owners, and
import-introduced identities at the host-compilation boundary. For an allocator, occupy its first candidate in
the same namespace in an emitter unit or mutation test; for a reserved
identity, pair the natural artifact fixture with a mapping/parser test proving
no legal Kio name can reach it under host equivalence. Exact private helper
spellings and other no-filesystem invariants stay in unit/mutation tests; do
not make a golden or emission implementation-shaped merely to force a private
collision.

Add at least one `HOST_INTERFACE` emission for the new backend. Its fixed
checked-in `host/`, independently authored from the public backend spec,
exercises semantic selector collisions: a value `child`, nested module `child`,
and public newtype `Child`, plus distinct module paths whose candidate host
renderings collide under the host language's identifier equivalence (for
example, a source underscore versus a path separator). Compile and navigate
the emitted artifact from that natural host code. The host must not be
generated, repaired, or extended by reading current output. Expected public
selectors may pin the documented ABI; private helper spellings may not.

Also prove non-injective opaque host-type binding with evidence appropriate
to the facade. For configurable type entries, compile a host that binds two
compatible Kio `host type` declarations to the same concrete host type or
type constructor in the `HOST_INTERFACE` emission. Reuse a runtime protocol
when one naturally crosses both declarations, but do not invent a runtime
golden merely to prove that the host typechecker accepts equal bindings. For
fixed or erased mappings, use focused emitter unit/mutation evidence that two
declarations may share the documented
representation and that no nominal wrapper is synthesized. In every form,
separate declaration lookup remains intact.

Every emission is a direct child of the new backend bucket and has exactly one
empty `HOST_INTERFACE` or `ARTIFACT_SHAPE` marker; the filesystem is the
registry, so add no marker catalogue. Use `ARTIFACT_SHAPE` only when the backend
spec or a recorded optimization/resource measurement makes the exact path,
manifest, language mode, named public file, or portable artifact proxy durable.
Do not pin incidental formatting, helper names, or declaration order. Follow
[`test-data/emissions/README.md`](../../../test-data/emissions/README.md): one
root package and target matching the bucket, success-only `run.sh`, no
`run.args`/`oracles`/`KNOWN_FAILING`, immutable checked-in `workdir/`, and a
scratch copy below `$TMPDIR` for every build.

Before calling the corpus upgraded, run a **backend-wiring audit** over all
runtime packages, not just the standard `run.args` path:

- `test-data/goldens/**` cases with `run.args` **and** custom `run.sh`
  scripts; custom scripts often carry their own manifest under `workdir/`
  and can silently omit the new backend.
- `test-data/poc/*/workdir/*.pkg.kio`.
- `test-data/castles/*/workdir/*.pkg.kio`.

For each package that declares the runtime target set, the new backend must
be present. A genuine impossibility is a blocking caveat, not an opt-out. Then
run a selection audit that proves the orchestrator actually selected the new
backend for the cases meant to cover it. A matrix summary such as "`kio@<lang>`
passed N cases" is not sufficient unless the audited manifest set shows the
backend was listed in every in-scope runtime case, including custom-script
goldens.

Audit the emission bucket separately: every case has exactly one build target,
and it is the new backend. Run the direct emissions orchestrator with an exact
`^<lang>/<case>$` selector while iterating and with all cases for acceptance.
Direct/local broad runs cover all emissions by default; normal GitHub sampling
uses `--sample-cases`, whose emissions default keeps one case per available
backend. The equivalent explicit override is
`--case-coverage=emissions:1`.

The trap to avoid is **build-only / false coverage** — the
`run.sh` / `$KIO_TARGET` false-coverage trap. A golden that lists the backend
in `build { ... }` but is driven `construct-only` or `kio
check`-only proves the shape *compiles*; it does **not** prove the value
*crosses at runtime*. Coverage requires a protocol that runs the emitted
artifact (a `-main` tier, a testapi protocol, or a roundtrip) and actually
moves the shape across the boundary. § The completeness gate is what enforces
this — it is the audit that fires on a silently-absent or build-only cell.

A green `HOST_INTERFACE` or `ARTIFACT_SHAPE` case does not change that result:
emissions supplement the runtime matrix and never turn a build-only or absent
cell green.

When authoring or editing a golden, read
[`ai/topics/kio-authoring.md`](../../topics/kio-authoring.md) first (trigger:
editing a `.kio` file). Goldens demonstrate what a user would naturally write
— never reshape the source to dodge an emitter gap (§ Before the first
emitter line). Run the affected cases against the new backend with
`--impls=FULL_IMPL_MATRIX` or an explicit list per [`ai/topics/local-ci.md`](../../topics/local-ci.md)
§ Local gate coverage.

When authoring an emission, read the same topic plus
[`test-data/emissions/README.md`](../../../test-data/emissions/README.md). Use a
backend-first exact selector; do not add its single backend target to every
other emission bucket.

## Step 6 — Spec, docs, devcontainer, CI

The non-emitter deliverables, each with a sweep:

- **`specs/backends/<lang>.md`** — the host-author contract, following the
  **9-section structure** ([`ai/topics/specs.md`](../../topics/specs.md) §
  What goes into a backend spec, swept by
  [`audit-backends-shape`](../audit-backends-shape/SKILL.md)): (1) a
  `Host API stability: evolving` field immediately below the title, an
  adjacent link to the shared Host API stability contract, plus a **family**
  declaration (`Family: <name>`), with no backend version, maturity tier,
  other stability label, or caveat banner, (2) concrete ecosystem-native
  public language floor, stable gating features, and any stable
  source/manifest language modes they require (not the exact repository
  toolchain resolution or a floating "latest" claim), (3) output layout, (4)
  loading protocol, (5) package API,
  (6) host record contract, (7) FFI surface (atomic / structural / carve-out
  categories), (8) item naming / mangling rule, (9) worked example.
  Every applicable universal or semantic-predicate shared property is
  **referenced** from
  [`specs/backends/README.md`](../../../specs/backends/README.md) by section
  number, never restated; the page records only the current host realization.
  Add the page to the § Backend pages list and the § Language families members
  column. Add the link to
  [`ai/topics/specs.md`](../../topics/specs.md)'s `specs/backends/` bullet.
- **`docs/hosts/<lang>.md`** — the narrative companion for a host author
  (trigger: [`ai/topics/docs.md`](../../topics/docs.md); the authoritative
  contract is the spec page, the guide may omit detail but must not
  contradict it). It carries the same `Host API stability: evolving` field
  immediately below the title, followed by an adjacent link to the shared
  contract, and no backend version, maturity tier, or other stability label.
  Normal acceptance has no caveat banner; if the user
  separately authorizes an exceptional caveated backend after the skill stops,
  its concrete top banner must match the spec page. **Altitude guard:** you
  just classified this backend's family and boundary machinery for the emitter
  — do *not* carry that framing into the guide. A host author embeds Kio in one language and cares about
  *what they see and do at the boundary*, not where the backend sits among
  Kio's families or how it contrasts with erased / native-HKT hosts; leave the
  taxonomy to a link and apply [`ai/topics/docs.md`](../../topics/docs.md)'s
  "why should a user care?" test to every sentence. Add the link to
  `docs/README.md` § Host integrations.
  The page's host-language fences are compile-checked by
  [`ci/checks/orchestrators/host-docs-snippets.sh`](../../../ci/checks/orchestrators/host-docs-snippets.sh):
  its `kio` fences must assemble into one buildable example package (a
  `{file}` support set plus a file-backed-harness `main`, not `{ignore}`),
  and every host fence must compile against the built facade — run the
  script before calling the page done.
- **Host API stability validation** — run
  [`ci/checks/repo-lint/backend-api-stability.sh`](../../../ci/checks/repo-lint/backend-api-stability.sh).
  It derives the backend set from the spec pages, requires a matching guide and
  exact field on each, enforces the `evolving` introduction default and
  rename-as-remove/add semantics, rejects a field introduced after its backend
  pages outside the initial policy rollout, and checks the explicit transition
  record. Backend admission does not promote the field.
- **Backend list / count sync** — the canonical backend set is still
  `specs/backends/*.md` excluding the README. Keep the top-level `README.md`
  supported-host-language count in sync with that set and link it to both
  `docs/hosts/` and `specs/backends/`. The website derives its host-language
  list from the same backend set and `docs/README.md`; run
  `cd website && npm run prepare:docs && npm run audit` after adding the
  host guide. [`audit-website`](../audit-website/SKILL.md) is the
  mechanical backstop for this count/list sync.
- **Toolchain provisioning** — start with a root `mise.toml` entry for the
  host compiler/runtime or the concrete toolchain pieces the runner needs.
  Declare the exact development/CI resolution in the mise configuration and
  regenerate `mise.lock` where that backend supports locking. If the mise tool
  is an installer for a delegated sub-toolchain, pin both the installer and the
  actual compiler/runtime in the visible configuration or install hook; do not
  pretend the lock resolves the delegated version. Do not substitute the
  public floor for this reproducibility resolution. The exact resolution runs
  the backend corpus, while the admission floor toolchain or a mode that
  truthfully enforces it compiles the emitted artifact and complete host-fence
  set with exactly the ordinary stable source pragmas, manifest/edition
  declarations, and language modes named by the floor. Feed every
  source/language-version flag or generated manifest/directive that can affect
  compilation into the runner's build-cache identity.
  Prefer mise core/registry names first, then explicit binary backends
  (`http:`, `github:`, `aqua:`, `conda:`, `pkgx:`) before any bespoke
  shell. If the backend needs a sub-toolchain managed by an upstream installer
  (the standing example is `ghcup` installing GHC), keep that command as a
  visible mise postinstall hook or a Dockerfile escape hatch with the pinned
  version in `mise.toml`. Source builds are not an acceptable silent default.
  A Dockerfile / workflow install step is an escape hatch only after a
  concrete no-source-build mise/direct-binary route failed or was rejected,
  and it carries a one-line reason. A compiler installed by such a manager
  lands *outside* mise's shims (ghcup puts `ghc` in `~/.ghcup/bin`), so its
  bin directory must be on PATH in **all three** environments that run the
  runner — the `.devcontainer/Dockerfile`, `INSTALL.md` for local checkouts,
  and agents' `BASH_ENV` snippet (see
  [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Toolchains on
  PATH). A runner that fails *wholesale* — every case empty-output / exit 1
  — is this missing-toolchain PATH gap, **not** a broken runner: fix the
  PATH; never disable the backend or call the runner "broken" (`AGENTS.md` §
  Universal rules — "Bugs surface; never hide them").
- **`.devcontainer/Dockerfile`** — add only the system packages or
  escape-hatch install steps the mise route cannot cover. The Dockerfile pins
  mise and consumes root `mise.toml` / `mise.lock`; it is not where ordinary
  language tool versions belong.
- **CI** — add the backend to the enumerations that fan out the matrix: the
  `ALL_IMPLS` list and the per-backend loop
  in [`ci/checks/orchestrators/generative-tests.sh`](../../../ci/checks/orchestrators/generative-tests.sh)
  (and the sibling orchestrators that name the backend set). Add its bucket to
  the emissions orchestrator's available-backend derivation without adding a
  separate marker registry. Local/direct emissions stay all-cases by default;
  the normal GitHub sampled lane selects one case for every available backend,
  not one global emission. Update the
  portability job's mise install arguments for any hosted-runner OS with a
  proven binary route. If a runner OS lacks one after trying explicit mise
  backends and a direct binary download, exclude that backend/platform cell
  explicitly and do not count the exclusion as portability coverage. Confirm
  the paired-jobs and bucket rules in
  [`ai/topics/repo-layout.md`](../../topics/repo-layout.md) when touching
  `ci/`. (The runner is one crate already in the version mirror; no
  `ci/checks/repo-lint/version-check.sh` change is needed for a runner
  *binary*.)

## The completeness gate (hard acceptance)

**A backend is not done until
[`audit-backend-completeness`](../audit-backend-completeness/SKILL.md)'s matrix
for the new backend's column is FULLY GREEN.** This is the hard gate — run it
as the final acceptance step and treat any red or caveated-impossible (`I`)
cell in the new backend's column as a blocking failure, the same status as a
failing golden. The audit
already documents this contract in its § 6 How `/add-backend` gates on this
audit; honor it exactly.

Concretely, the definition-of-done is:

- **Every shape axis** (the audit's § 1: scalars/roles, product, sum,
  newtype, closure/function value in **both directions**, multi-group /
  curried closure, rank-N, existential, host-owned generic type, HKT
  carrier, functor/monad dictionary) has a **runtime-exercising** golden
  whose `build { ... }` lists the new backend and whose protocol crosses the
  shape in each applicable direction (the audit's § 2: host→package env and
  package→host export).
- **No build-only, no absent, no stubbed cell** in the new column.
  "The backend builds the corpus" is necessary but **not** sufficient — the
  audit additionally proves every shape *crosses at runtime*, which a
  build-only pass does not. This is what makes "a build-only / stubbed shape
  can never hide" structural rather than a matter of remembering to check.
- **Emission evidence is complete but separate.** The new backend has an
  independent `HOST_INTERFACE` case, plus `ARTIFACT_SHAPE` cases only for
  durable spec/measurement facts. No emission is credited to a runtime cell.
- **The internal polymorphic application-stage witness is green** for the new
  backend: adjacent direct type arguments, a singular type application on a
  returned or computed value, adjacent type applications on one first-class
  value, and observable evaluation before a later application all run with
  their occurrence-appropriate realizations.
- **The recursive control-flow stack-safety witness is green** for the new
  backend: a deep finite `rec(loop)` carrying a product and function-valued
  state slot completes at the documented fixed depth. Its RED/GREEN evidence
  and causal guard distinguish exact carried-representation reuse with no
  pending work from same-ABI real conversion, different-ABI adaptation, and
  genuine pending continuation work.
- **Backend selection is complete.** The backend-wiring audit from § Step 5
  has no missing runtime manifests, including custom `run.sh` goldens whose
  manifests are not updated by the standard `run.args` bulk edit.
- **Any shape the host genuinely cannot express** is surfaced to the user as
  a blocking caveat. Even a correctly documented and discharged impossibility
  is not a green cell and cannot pass normal acceptance.

Run it scoped to the new column and block on the column being fully green (modulo
the audit's § 5.7 spec-admitted-not-mandated coverage gaps — the
HKT-boundary dictionary cell is the standing example; it is a corpus-wide
coverage decision, not a backend-specific conformance bar).

## The audit-set gate

The completeness gate is the headline, but a backend is accepted only when the
**full backend audit set** is clean, validated against the existing backends.
Run each and resolve its findings:

- [`audit-backend-completeness`](../audit-backend-completeness/SKILL.md) — the
  hard gate above; the only check that fires on a silently-absent shape.
- [`audit-spec-drift`](../audit-spec-drift/SKILL.md) **§ 7** — the
  per-backend carve-out **discharge**: it lists every claimed impossibility
  and forces the manual *genuine-vs-fixable* verdict against the escape-hatch
  standard. The only check that catches a **false** carve-out that passes
  every signal-based sweep. Also verifies spec-has / impl-doesn't and the
  degraded-support mutual-cite.
- [`audit-partial-implementations`](../audit-partial-implementations/SKILL.md)
  **§ 6** — silent-guard / catch-all fallthrough: a guard that narrows on a
  property of a spec-admitted shape and routes the constructible complement
  to an identity / pass-through arm, with no comment and no error.
- [`audit-generated-binder-hygiene`](../audit-generated-binder-hygiene/SKILL.md)
  — every private generated host/IR value, type, and import binding is outside
  its user-reachable namespace or uses a justified complete allocator, while
  every public facade identity has a stable injective semantic mapping, with
  alpha-renaming, declaration/use, nested-shadowing, and emitted-artifact
  collision evidence.
- [`audit-backend-family-conformance`](../audit-backend-family-conformance/SKILL.md)
  — the page declares its family and matches the family's shared idioms, or
  carries an explicit mutual-cited divergence in host realization while still
  implementing the full Kio contract. It also inspects representative emitted
  facades and real host call sites for human usability; a technically callable
  but opaque, flattened, cast-heavy, or non-idiomatic public API is a finding.
  It also audits cross-backend decisions against the semantic-applicability
  ledger; a family divergence cannot excuse an unsupported or degraded feature
  or an applicable peer left unassessed.
- [`audit-corpus`](../audit-corpus/SKILL.md) shared §§ 1–6 and Emissions 0–5 — the new
  backend is not quietly left out of the corpus (no per-backend opt-out
  lacking a mutual-cite, no deleted/bucket-bumped golden, no source reshape to
  dodge an inference gap, no absolute/checkout-local path), and its backend-
  first emissions obey the one-marker, independent-host/durable-artifact,
  scratch-build, and success-only contract.
- [`audit-backends-shape`](../audit-backends-shape/SKILL.md) — the spec page
  follows the 9-section structure and references the README rather than
  restating it.
- [`audit-runner-host-fidelity`](../audit-runner-host-fidelity/SKILL.md) —
  the runner constructs the host and invokes the package the way a real host
  would, compatible with the spec page — including independently deriving
  package-level names from harness-supplied package identity, never
  hardcoding them.
- [`audit-test-strategy`](../audit-test-strategy/SKILL.md) § Runner
  independence — the named protocol is the sole authority for signatures,
  shapes, exact typed-host declaration inventories, and fixture choices; the
  runner neither scrapes emitted artifacts nor accepts per-case semantic
  extensions that make emitter↔runner agreement tautological; golden-owned
  code never handles generated host files directly, while fixed-host emissions
  remain independent.
- [`audit-package-coexistence`](../audit-package-coexistence/SKILL.md) —
  the coexistence witness covers the new backend: `ffi_two_packages_coexist`
  declares it in both packages' build blocks and the runner's coexist arm
  genuinely hosts two live instances.
- [`audit-host-docs-snippets`](../audit-host-docs-snippets/SKILL.md) — the
  new host guide is covered by the snippet gate; every host-language fence
  validates, none demoted to dodge the check.
- [`audit-website`](../audit-website/SKILL.md) — the top-level README,
  `docs/README.md`, generated website language data, and landing page all
  agree on the derived host-language set and count.
- [`audit-no-leak`](../audit-no-leak/SKILL.md) — the new backend's files
  (spec, docs, source, CI, comments, commit messages) leak no
  session/machine/user context.

### The load-bearing trio

Three of these together make "a stubbed / opted-out / false-carve-out shape
can't hide" a **structural** property, not a matter of vigilance. State this
explicitly when reporting acceptance:

- **completeness** finds the **absent / build-only cell** — the shape that
  compiles, never crosses at runtime, and carries no comment, no error arm,
  no carve-out. None of the signal-based sweeps fire on it; this audit is the
  only one that does.
- **spec-drift § 7** discharges the **impossibility claim** — a *false*
  impossibility carve-out (a documented "Limitation" paragraph + an
  `EmitError` / `unreachable!` arm over a spec-admitted shape) passes every
  signal-based sweep; the manual per-carve-out discharge is the only thing
  that catches it.
- **partial-impl § 6** finds the **silent guard** — the catch-all
  identity-pass with no "unsupported" phrase, no error return, that none of
  the phrase greps catch.

Each closes a gap the other two leave open: an absent shape (no signal),
a false carve-out (signals present but the *claim* is wrong), a silent
guard (no signal, complement constructible). Drop any one and a whole class
of "looks done, isn't" hides. **All three must pass for the new column.**

## The adversarial review

After the corpus is green and the audit set is clean, run an **adversarial
review** of the new backend — a fresh-eyes red-team pass that assumes the
backend is *not* done and tries to prove it, as the swift and haskell
pilots did. Spawn a subagent (or take the
adversary role explicitly) and hunt for:

- **A shape that compiles but is never run** on the new backend — pick each
  audit § 1 shape and trace it to a *runtime* golden + protocol that crosses
  it on this backend; a `build { ... }` listing without a crossing protocol is
  the false-coverage trap.
- **A guard that narrows and identity-passes the complement** — for each
  `convert_*` hook and body arm, name the constructible shape it routes to
  the catch-all and find the golden that distinguishes a correct emission
  from an identity pass.
- **A carve-out whose impossibility is false** — for each "Limitation" both
  sides, re-discharge against the escape hatch: is the safe / idiomatic path
  genuinely exhausted, or merely not attempted?
- **A reshaped golden** — a `.kio` source carrying an extra annotation, a
  monomorphised newtype, or alias scaffolding that exists only to dodge this
  backend's gap.
- **A forgeable generated binding** — for an allocator, choose the emitter's
  first candidate as a legal user binding in the same actual host namespace;
  for an import-introduced identity, classify that namespace as value, type,
  module, package, or another namespace according to host resolution. For a
  reserved identity, challenge the grammar/mapping proof with the nearest
  legal spellings and the host's decoding/normalization rules.
  Test item-level and local values, backend type parameters, generated
  alias/nominal owners, and import-introduced identities; nest shadowing
  bindings and verify declarations and uses still resolve to their intended
  identities after compilation. Do not treat a non-binding property/shape key
  as evidence for a binding family.
- **A family divergence with no mutual-cite or one that weakens the full Kio
  contract**, a leaked path / identity in a checked-in file, a runner host body
  that diverges from how a real host would call.
- **A facade identity flattened before rendering** — exercise a value `child`,
  nested module `child`, and public newtype `Child`, plus other distinct
  selectors whose candidate host renderings collide under the host language's
  identifier equivalence, such as source underscores with module separators;
  then add an unrelated declaration and confirm every pre-existing public
  selector is byte-stable. Reject declaration-order suffixes and APIs a human
  host author cannot reasonably navigate.
- **An injective opaque-host mapping disguised as type safety** — on a
  configurable facade, bind two compatible exact declarations to the same
  concrete host type or type constructor. On a fixed or erased facade, trace
  two declarations through the documented mapping without inventing
  configurable slots. Any generated wrapper, brand, tag, defined type,
  `NewType`, or marker whose only purpose is to keep their host
  representations distinct is a finding; separate declaration lookup must
  remain intact.

Treat anything the adversary finds as a blocking finding: fix the emitter /
runner / corpus (never hide the shape), then re-run the completeness gate and
the trio. Report what the adversary probed and what it found.

## Hard-won lessons (concrete pitfalls from the pilots)

Bake these in — each is a real bug a shipped pilot hit:

- **Per-module name resolution, not bare-leaf.** Resolve host items and
  shape keys per **declaring module**, never by flattening to the leaf name or
  by encoding `(module, leaf)` into one string that later code must split —
  the boundary is module-qualified (§ 8 Namespace preservation). The Haskell
  pilot's **newtype-collision bug** came from leaf-name flattening: two
  modules' same-named newtypes collided. Two `host fn print`s in different
  modules are distinct entries; a naive-flat rendering that collides is
  forbidden.
- **Nested-product handling at fn FFI slots (right-spine arity).** A function
  parameter that is itself a nested product flattens by the **right-spine**
  walk for positional arity, exactly as a top-level signature does
  (§ Function-type FFI canonicalization). Don't special-case the top level
  and mishandle a product nested at a callback slot.
- **Genuine runtime coverage, not build-only** — the `run.sh` / `$KIO_TARGET`
  false-coverage trap. A golden that builds for the backend but is driven
  construct-only or `kio check`-only never runs the artifact; the shape is
  *undetected, not unsupported*. Coverage means a protocol that runs the
  emitted artifact and moves the shape across the boundary (§ The
  completeness gate).
- **Emission evidence is not runtime coverage.** A fixed host that compiles or
  loads the public artifact and an artifact-shape check answer important
  backend-specific questions, but neither closes a language/runtime matrix
  cell. Keep those cases backend-first under `test-data/emissions/<lang>/` and
  keep golden-owned code away from generated host files. The specified Kio'
  phase-artifact exception does not extend to backend output.
- **Custom `run.sh` manifests are easy to miss.** A custom-script golden may
  assemble a temporary package from a private `workdir/*.pkg.kio`; updating
  only the standard `run.args` packages leaves that golden unselected for the
  new backend while the matrix still reports a clean run over the cases it
  did select. The backend-wiring audit exists specifically to catch this.
- **Runner protocol surfaces must be complete even for uncalled capabilities.**
  A package can require a host item because it is part of the protocol surface
  even when a particular execution path never calls it. The runner must
  synthesize a loud body for every required item so host construction itself
  is faithful.
- **Multi-group currying — closures AND module-fn-values.** Curried
  multi-layer functions stay curried at the boundary (each `Function(Pᵢ, Rᵢ)`
  = one call). This holds for **both** a closure value and a module-fn value
  carrying multiple parameter groups; cover both, in both directions.
- **Representation is not application-boundary authority.** A representation
  is wrong when it loses semantic `Forall` / `Function` order required by the
  current applicable contract.
  Prove four distinct shapes on the actual runtime: adjacent direct type
  applications; a singular type application on a returned or computed value;
  two adjacent binder applications on one first-class polymorphic value even
  when both are work-free; and observable work before a later application.
  Establish the realization separately for each occurrence; never infer it
  from family or body storage. Constructor/projector identities wrap the whole
  payload rather than flattening its semantic application boundaries.
- **The false-carve-out worked example (why § 7 is non-negotiable).** The
  Haskell pilot's **functor-dict-at-boundary** was initially claimed
  *impossible* — a host fn whose parameter/return is a functor/monad
  dictionary. It read as a documented limitation with a matching emitter arm,
  so it **passed every signal-based sweep**:
  `audit-partial-implementations` § 6 excludes `unreachable!` / `Err` arms,
  `audit-corpus` saw no deleted golden, the mutual-cite was intact.
  Only the **`audit-spec-drift` § 7 escape-hatch discharge** caught it: the
  dictionary is an ordinary `fn`-parameter value (Kio has no typeclasses), so
  it falls to the uniform-`m` floor like any other residual — exactly as
  every other shape that isn't a typed host shape does. It was **fixable, not
  impossible**, and the carve-out was false. This is the cautionary tale: a
  plausible-looking impossibility is exactly what the signal sweeps cannot
  refute, and the § 7 discharge is the only thing standing between a false
  carve-out and a green build. **Never skip it.**

## Acceptance and reporting

The backend is accepted only when **all** of the following hold:

1. The emitter, runner, spec page, host guide, backend-first emission bucket,
   devcontainer toolchain, and CI enumerations all land together (`AGENTS.md` § Universal rules — "Language
   changes move the whole surface together").
2. The corpus carries genuine **runtime** coverage for the new backend — every
   shape axis crosses at runtime, no build-only or absent cell, and the
   backend-wiring audit shows the new backend is listed in every in-scope
   runtime package (standard and custom-script cases).
3. **The completeness gate is fully green** for the new column, and **the
   load-bearing trio** (completeness + spec-drift § 7 + partial-impl § 6) all
   pass — plus the rest of the audit-set gate.
4. The generated-binding naming plan, adversarial collision fixtures, and
   `audit-generated-binder-hygiene` result are clean; the human-usable-facade
   audit is also clean against representative independently authored
   `HOST_INTERFACE` call sites. Any `ARTIFACT_SHAPE` case pins only a durable
   spec- or recorded-measurement-backed fact, and emissions were not counted as
   runtime cells.
5. The runner's protocol host-body completeness gate passes.
6. Every native compiler route uses shared `CompilerAdmission` at the actual
   command boundary; cache hits and same-key waiters consume no permit.
7. The **adversarial review** ran and its findings are fixed (never hidden).
   Its opaque-host probe uses the facade-appropriate evidence from Step 5; it
   does not require a new runtime protocol solely for type-binding
   admissibility.
8. No known caveat remains. A genuine impossibility has been surfaced to the
   user and the normal acceptance workflow has stopped before landing.
9. The coexistence golden is green on the new column
   (`ffi_two_packages_coexist` runs the backend's coexist arm), and
   `ci/checks/orchestrators/host-docs-snippets.sh` passes with the new
   host guide included.
10. Sealed signature-history behavior has paired source evidence: an old host
    still compiles wherever the backend claims source stability; a new host
    supplies and selects live items only; every emitted retained-only
    declaration is visibly deprecated; a live-plus-history declaration
    remains live; and no retained item enters loader/runtime/package dispatch.
    If the host cannot express an optional deprecated shim, or incompatible
    frozen epochs cannot share one exact declaration identity, the backend
    omits the affected root and closure and publishes the concrete
    source-compatibility caveat instead.
11. The release decision uses only capabilities in the maintained GA/final
    stable surface, available through stable defaults or the ordinary stable
    source pragmas, manifest/edition declarations, and language modes named by
    the floor, across every runtime/toolchain class the backend admits. No
    preview or unstable experiment carries the contract; the concrete public
    floor compiles the emitted artifact and complete host-fence set with those
    exact stable settings, the exact repository resolution runs the backend
    corpus, and neither is represented as an ambient moving "latest" target.
12. The backend performance review is clean: preparation and emission do not
    repeat per artifact or selected target without necessity; representative
    narrow and natural-wide packages show bounded generated size/nesting; and
    the real host compiler has recorded focused wall/RSS evidence. Any
    performance-only realization has a causal firing/no-op guard and measured
    benefit, while a rejected optimization leaves no production complexity.
    Deep finite `rec(loop)` execution also passes the causally established
    fixed-depth host-stack witness on the backend, including structured state
    with a carried function slot.
13. The backend spec and host guide carry matching `Host API stability:
    evolving` fields, and the backend API stability repo lint passes. The
    compatibility status supplies no credit toward any preceding acceptance
    item.

If a slice is genuinely incremental, say so and leave the tracker open
(`AGENTS.md` § Universal rules — "No partial implementations") — do not call
the backend done with a red or caveated cell papered over. If a shape can't run
on the new backend, the case stays as a failing golden (the bug surfaces, it is
not hidden) and the backend does not land through this skill. If the host can
express the shape, fix the emitter; if it genuinely cannot, stop and surface
the caveat to the user.

Report: the release decision (stable line, ecosystem-native public floor,
gating features and stable source/manifest language modes, exact repository
resolution including delegated sub-toolchains, and floor/resolution evidence),
the classification (authorized family + body/storage regions + public
type-relationship occurrences), the semantic applicability ledger and shared
destinations, the per-backend deliverables landed, the shared-infra plug-in
points used, the completeness matrix for the new column, the backend-wiring
audit result, the backend-first emission cases and their markers/contracts, the generated-binding naming plan and collision fixtures, the
runner host-body completeness result, the human-facing facade evidence, the
audit-set results with the trio called out, the backend performance review and
focused source-size/host-compiler wall/RSS evidence, and the adversarial
review's probes and findings.

**Default: report only is not an option for this skill — it lands a backend.**
But when invoked against an audit with a fix directive on findings, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) for the
commit discipline (logical commits, test before each, don't sign off or
push).

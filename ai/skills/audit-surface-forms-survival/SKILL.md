---
name: audit-surface-forms-survival
description: Verify Kio' stays a strict subset of Kio — no Prime-only extension, surviving surface form, hidden provenance, or producer-derived authority; evaluation and recovery consume self-validating phase artifacts
allowed-tools: Read, Grep, Glob, Bash
---

# Kio' boundary audit

AGENTS.md § Universal rules — Kio' is a strict subset of Kio, Surface forms must not survive into Kio', Calls are typed from their resolved types, never declaration side channels, Phase artifacts cannot inherit authority from their producer, and Compile-time evaluation observes phase boundaries are the headline rules. This skill audits the kio-rs front-end, phase-artifact authority, compile-time evaluator entry points, and post-typecheck recovery pass to verify seven related properties:

1. **Kio' adds nothing to Kio.** Every spelling, identifier class, declaration/import form, typing rule, and runtime meaning accepted as Kio' is accepted with the same meaning by the full Kio pipeline. Kio' is a restriction, never a privileged compiler dialect.
2. **No surface-only form survives into Kio'.** The AST that reaches the Prime phase contains only Kio' constructs.
3. **No surface-only *information* survives into Kio'.** Even when a surviving node looks Kio'-shaped, it must not carry hidden surface provenance — original label-declaration names, "was the signature elided" flags, optional types that encode "user wrote it" vs. "typer filled it in," or any other back-reference whose only consumer is "remember which surface form produced this." Such fields are themselves hidden surface forms.
4. **Compile-time evaluation consumes phase-pure inputs.** `equiv`, `:normalize`, user elaborators, generated-term templates, and future `__comptime__` hooks reduce Kio'-shaped core terms plus an explicit primitive environment. In kio-rs, an entry adapter may use typed Lowered plus boundary-local `Elaborations` only to build an explicit `UncheckedPrime` evaluator input; the reducer must never consume Lowered/surface ASTs, `Elaborations`, or hidden typer state.
5. **`structural_recovery` consumes Kio' shape only.** The recovery pass (`Module<Prime>` → `Module<Enriched>`) is the back-end's only mediator between Kio' and codegen. Its decisions must be driven by intrinsic call structure, type-args, and Kio'-identity names — never by surface-provenance fields that rode through Prime so recovery could read them.
6. **Phase artifacts derive no authority from producer provenance.** A Prime/textual/serialized/cache/dynamic-load construct may grant only authority that its receiving validator can verify from the artifact and explicit dependencies. Reserved or compiler-generated spellings provide hygiene only, and an exact selected private edge never expands into module-wide access.
7. **Call semantics use resolved types only.** Derive call completion, argument grouping, partial application and residualization, and UFCS placement from the resolved public type, written syntax, and local constraints. Never use declaration or host-parameter grouping, runtime ABI arity, callee identity or provenance, or a private elaborator ABI to change the public call plan.

Property (3) is the generalization of property (2): a `from_labels_label: Option<String>` on a Prime-surviving `Newtype` is conceptually a surviving `labels` declaration, just expressed as a field instead of an AST variant. Before label elaboration, explicit `Labels` declarations and Surface label relations remain authoritative through Surface transformations and dependency materialization. Their erasure forbids carrying that provenance into Prime; it does not authorize reconstructing a Surface label binding from an ordinary newtype. Property (4) extends the same phase-boundary purity rule to evaluator inputs. Property (5) is the motivation: when a downstream pass cheats by reading surface provenance, the front-end's layered design erodes — recovery becomes coupled to the front-end's history rather than the Kio' input it claims to accept. Property (6) applies the same producer-independent counterfactual to authority: a fresh parser and validator see the artifact, not who emitted it.

## The dump-and-reload counterfactual

The decisive test for property (2): **would the field still carry its value after dumping Kio' source and reloading it through a fresh parse + typecheck?** The counterfactual reverses the question — instead of asking "is this surface-only?", ask "is this information present in the Kio' source the kio-prime backend would emit?"

Two outcomes:

- **The field would survive the round-trip.** It's encoded in the Kio' source text (a name, a structural shape, an arity the spelling pins). A fresh load would re-derive the same value. The field is genuine semantic info — even when it carries a *surface flavour* like "the user wrote N comma-separated params," the value is recoverable from the source. Keep the field.
- **The field would not survive the round-trip.** The Kio' source carries no encoding of it, so a fresh load synthesizes a different (or arbitrary) value. The field is hidden state — surface provenance that survives as a phase-boundary leak. Narrow it.

The counterfactual is sharper than "is this surface-only?" because it cuts through cases where a field *looks* like surface provenance but actually corresponds to a syntactic distinction Kio' itself preserves, such as optional lambda parameter annotations. Kio' source must be able to re-derive the value after dump and reparse; if it cannot, the field is hidden provenance and must be narrowed.

Apply the counterfactual to every field flagged in step 7 below: if a fresh-load round-trip recovers the value, the field stays; if not, the field is hidden surface provenance and narrows.

### Phase-boundary side channels

The counterfactual rules out one obvious cheat (surface-provenance fields on the AST). It also rules out a subtler one: **a parallel data structure that carries the same info around the AST instead of through it.** A `HashMap<NodeId, Type<P>>` consumed by codegen is just as much hidden state as a `ty: Option<Type<P>>` on `Param` — the AST looks clean, but the information still flows from the typer to the back-end via a cross-boundary side channel.

Use the vocabulary precisely:

- **phase-boundary purity** — downstream phases consume only the artifact declared for that phase.
- **phase-boundary leak** — a surface/history/typer-only fact is observable after the boundary closes.
- **boundary-local side channel** — temporary scaffolding consumed before the boundary closes by writing into the next-phase AST.
- **cross-boundary side channel** — forbidden parallel data consumed after the boundary instead of being represented in the phase input.

Reserve **no-leak** and **context leak** for session, machine, user, worktree, and scratchpad leakage in checked-in artifacts.

The discriminator:

- **Typer -> substitute workspace, consumed by writing into the AST.** Legitimate. `Elaborations` is the canonical boundary-local side channel: the typer records the next-phase shape for elaboration-bearing sites, the substitute pass reads it and writes the explicit `__either__`/`__if_then_else__` calls or checked user-elaborator replay into the Prime AST, and after substitute the side table is dead. The final Prime AST is the public encoding; the side channel was scaffolding.
- **Typer -> codegen, evaluator, or any pass past substitute, consumed in parallel with the AST.** Forbidden. Whatever the parallel structure carries is information the downstream pass needs but the AST doesn't admit — which means the AST is incomplete. Either the AST should carry the info, or the pass should re-derive it from what the AST does encode (the same source text a fresh re-parse + re-typecheck would see).

The counterfactual is the test for both: if the dumped source carries the info, the AST should too; if it doesn't, neither should the side channel. A side channel that lives past the substitute boundary is the same hygiene failure as a Prime-surviving provenance field, just better-hidden.

When considering a narrowing fix that requires "the typer fills the AST via a side table during substitute," ask whether the value is canonically derivable from the surrounding context at re-parse. If yes — as for an inferred `Param` type, which the typer's bidirectional flow re-derives from context on every reload — the side channel is redundant scaffolding pretending to be data: prefer leaving the field's optionality on the AST as a genuine syntactic distinction ("this slot is inferred from context") rather than filling it via a cross-boundary side channel.

Read AGENTS.md § Universal rules — Kio' is a strict subset of Kio, Surface forms must not survive into Kio', Phase artifacts cannot inherit authority from their producer, and Compile-time evaluation observes phase boundaries, `specs/prime.md`, and the module doc of `kio-rs/src/pass/structural_recovery.rs` first.

## 1. Prove the strict-subset relation

Inventory every syntax production, identifier class, phase-extension variant, and semantic privilege admitted by the Prime parser, AST, lowerer, checker, or standalone verifier. For each one, identify the same spelling and meaning in the full Kio pipeline. Sharing a parser is not sufficient when a parser mode, post-parse check, AST extension, or checker branch changes what the construct means or who may use it.

Compare `specs/prime.md` and `specs/language.md`, the full and Prime parser entry points, the phase-polymorphic AST definitions, `prime::lower`, and the standalone Kio' verifier in both directions. Probe representative forms through both public entry points: every Kio' success must also be a full Kio success with the same static and runtime meaning. Flag any Kio'-only production, identifier class, declaration/import form, AST inhabitant, typing privilege, visibility bypass, or runtime behavior. Compiler generation and reserved spelling are not exceptions.

## 2. Enumerate surface-only forms

The surface forms that must not survive are:

- Tuple literals (n-ary `(a, b, …)`)
- Generic trailing block calls to imported elaborators (including `if!`/`else`, `scope!`, `do! <receiver> { … }`, and `match!`)
- Elaborators (algebraic): `iso!`, `into!`, `onto!`, `align!`, `ease!`, `atom!`
- Elaborators (spine): `fit!`, `reorder_sum!`, `reorder_prod!`, `narrow_sum!`, `narrow_prod!`, `widen_sum!`, `widen_prod!`, `flatten_sum!`, `flatten_prod!`, `one_sum!`, `one_prod!`
- Bang-call with explicit target type (`elaborator!(e, T)`)
- UFCS (`r.>m`, `r.>m(T)`)
- `fn#` placeholder lambdas
- `alias` literal aliases
- Label sugar (multi-label shorthand, `labels`)
- Nonminting label forwarding (`type {name} = {target.name};`)
- `op` user-operator bindings
- `equiv` blocks

For each, confirm `specs/language.md` documents the surface form and `specs/prime.md` confirms Kio' does not include it.

## 3. Prime AST shape

Read `kio-rs/src/prime/` (the Prime AST and its types). The Prime AST should contain no variant whose construction requires any surface-only form. For phase-polymorphic AST nodes used at both Surface/Lowered and Prime:

- The phase-extension field for Prime should be `Infallible` (or equivalent uninhabited type) for every variant that represents a surface-only form.
- The shared helpers in `typecheck_core` recurse via `Typer<P>` — confirm `PrimeTyper` discharges every elaboration arm via `match *ext {}`.

Flag any Prime AST variant that admits a surface-only construction.

## 4. `prime::lower` rejects surface forms

Read `kio-rs/src/prime/lower.rs` (or wherever `prime::lower` lives). For each surface form enumerated in step 2, confirm the lowering pass rejects it with a parse error. Cross-reference against the kio-prime binary's behavior: every surface-only form should produce a parse-error exit code matching `specs/exit-codes.md`.

If any form is silently passed through, that's the finding.

## 5. Elaboration substitution at the Lowered → Prime boundary

Generic trailing block calls are projected before desugaring according to ordinary imported elaborator declarations: product blocks become independent values, thunk blocks become lambdas, and sequence blocks become nested ordinary bind calls. The resulting imported calls, including `if!`, `scope!`, `do!`, and `match!`, use the same boundary-local elaboration path as other user elaborators when their removal needs completed types. `typecheck_full` records the elaboration and substitution swaps it into the AST at the Lowered → Prime boundary; UFCS and field syntax also close at that boundary. Elaborator calls stay as elaboration variants rather than being folded into the intrinsic-scheme registry — this preserves the phase-typed elimination invariant (`PrimeTyper` discharges every elaborator arm via `match *ext {}`). Check:

- Every dispatch position that produces an elaboration records it in the `Elaborations` table.
- The substitute pass consumes every recorded elaboration and emits only ordinary nodes admitted by the shared Kio'/Kio subset.
- No `Elaborations` entry is dropped or skipped.

Look for `Elaborations`, `substitute`, `lower_post_typecheck`, or similar symbols in `kio-rs/src/pass/full.rs`, `kio-rs/src/pass/typecheck_full.rs`, and `kio-rs/src/pass/substitute/`.

## 6. Cargo feature gating

The `kio-prime` binary should exclude every kio-only module (`desugar`, `elaborator_registry`, `full`, `label_elab`, `op_fold`, `substitute`, `typecheck_full`) via Cargo feature flags. Read `kio-rs/Cargo.toml` and the relevant `#[cfg(feature = "...")]` annotations to confirm. A kio-only module visible from the kio-prime build is a leak.

## 7. Surface-provenance fields on Prime-surviving nodes

Walk every node type that survives into Prime (`Item::FnDef`, `Item::Alias`, `Item::Newtype`, `FnDef`, `Signature`, `Param`, `Newtype`, `TypeMember`, the package-file containers, …) and inspect its field list for **surface-provenance fields**: optional names, flags, or back-references whose only purpose is to encode which surface form produced the node.

Known examples in the current tree to use as exemplars (counterfactual-failures that motivated narrowing):

- `Newtype.from_labels_label: Option<String>` (historical, now removed) — back-reference to the originating `labels` group's label spelling. After label elaboration, the ordinary newtype shape survives the Prime round-trip; its Surface origin does not. Downstream consumers derive the lowered newtype's host key from `Newtype::ffi_key()` (the newtype type name). That key never establishes Surface label-ness or a braced binding: explicit `Labels` declarations and exact Surface label relations remain authoritative before erasure, including during dependency materialization.
- `FnDef.ret_elided: bool` — flag for "the return type was elided in the surface." At Prime, narrowed to `()` via `Phase::FnDefRetElided`: the value is uninhabited there, so the round-trip on the Prime AST is trivially preserved and the kio-prime emit writes the canonical `-> .` shape.
- `Labels.type_alias_name: Option<String>` — moot at Prime today (`Labels` is uninhabited), but flag any new analogue that moves to a Prime-surviving node.

Counterfactual-survivors (fields with surface flavour that round-trip via source text — keep them as-is):

- `Type::Function.abi_arity: usize` — runtime/signature packet metadata used to bind definition bodies and adapt host calling conventions. It is not type identity, and equal semantic function types need not preserve the same declaration grouping. Neither this field nor a boundary-local signature artifact may select source call completion, grouping, residualization, or UFCS placement; those decisions use the resolved type and written syntax alone. A signature artifact may be consulted only after that public call plan is fixed, for representation or body-binding work that cannot alter source semantics.
- `Param.ty: Option<Type<P>>` / `Expr::FnExpr.ret_ty: Option<Type<P>>` — `Some(t)` means the user wrote the annotation, `None` means the typer infers from context. Kio' admits both shapes (`.(x) { … }` and `.(x: T) { … }` are distinct source spellings), the pretty-printer respects the `Option`, and a fresh re-parse re-derives the same `Some`/`None`. The optionality is a genuine Kio' syntactic distinction ("this slot is inferred from context"), not hidden provenance. The only narrowing that would work for these requires filling `None` from a side table at substitute time — which the phase-boundary side-channel rule above rejects, since the inferred type is canonically re-derivable from the surrounding context at re-parse.

For each candidate field on a Prime-surviving node, answer in order:

- **Which surface form does it back-reference?** If the answer is "none — this is genuine Kio' identity (item name, member name, structural sum arm)," the field is fine.
- **The dump-and-reload counterfactual.** Would a fresh kio-prime dump → re-parse round-trip recover the field's value from the source text alone? If yes, the field corresponds to a genuine Kio' syntactic distinction; even when it has a surface flavour it isn't hidden provenance. If no, the field is hidden state that survived as a phase-boundary leak.
- **Which downstream pass consumes it?** Run `grep -rn '<field-name>' kio-rs/src/`. If nothing reads it past `label_elab` / `desugar`, the field has no purpose at Prime and is dead provenance.
- **Could the consumer compute the same answer from Kio' shape alone?** For every "yes," the field is a hidden surface form: strip it at the boundary that strips the surface form it references (typically `label_elab` or `desugar`), and rewrite the consumer to read the Kio' shape.

The bar is the same as for surface forms: if the information is not part of Kio''s formal identity, it does not belong on a Prime node. The counterfactual is the test for "part of Kio''s formal identity."

**Exemplar: `Type::Function.abi_arity` is not type identity.** Kio type syntax uses `&` for product domains, not comma-separated function-type parameters. Declaration grouping may still be needed to bind a definition body or express a host ABI, but a consumer that makes two equal function types accept different source calls because of that grouping is leaking syntax into type semantics. Inspect every semantic read of `abi_arity`, signature parameter groups, host-function parameter groups, and user-elaborator transcript metadata. Require same-public-type counterfactuals across ordinary, host, Required/Optional elaborator, bang, prefix, and UFCS calls. Boundary-local artifacts may carry representation data only after the shared public call plan has been selected.

### Recommended phase-narrowing pattern

When a field is genuinely needed in earlier phases but fails the counterfactual at Prime, narrow it with the phase machinery rather than letting it ride through:

- **Boolean / `Option<String>` provenance flags whose Kio'-canonical form is "no info"** → introduce a phase-associated type (e.g. `Phase::FnDefRetElided`); `bool` / `Option<String>` at the phases that meaningfully store it, `()` at Prime. A `PhaseBridge` impl translates the value across the boundary (`bool → ()` discards; `() → bool` defaults at the kio-prime emit boundary). The field becomes phase-erased at Prime rather than phase-polymorphic.

Cross-boundary side channels are **not** an acceptable narrowing tool — see the phase-boundary side-channel rule above. A field that would need substitute to fill it from a `NodeId` table at the Lowered → Prime boundary is signalling that the field's `None` is a genuine Kio' syntactic distinction, not provenance to strip.

A reasonable end state is that *no* field on a Prime-surviving node carries information the dump-and-reload counterfactual can't recover — either the value round-trips via the source text (in which case it stays on the AST), or it's phase-narrowed to `()` (in which case Prime → reload trivially round-trips the absence).

## 8. Phase artifacts do not inherit producer authority

Inventory every Prime, serialized, cached, emitted, reparsed, and dynamically loaded construct described as `internal`, `privileged`, `reserved`, or `compiler-generated`. For each one, establish:

- **Producer-independent meaning.** Fresh textual Kio' and the full compiler's emitted Kio' reach the same validator and give the construct identical semantics. No branch grants authority because a caller, parser mode, cache record, or flag says the compiler produced it.
- **Validator-checkable evidence.** Visibility or private-member authority is rederived from the artifact's explicit dependencies and declarations. A reserved name proves only that users cannot collide with a binding; it never proves the binding is entitled to private access.
- **Least authority.** Evidence for one selected declaration authorizes only that declaration and operation. A link created for one private operator implementation must not expose sibling private functions, aliases, newtypes, constructors, projectors, or submodules.
- **Persistence parity.** Direct compilation, dump → fresh parse → Prime check, cold and cache-hit reload, and dynamic loading accept and reject the same authority edges. A cache may memoize prior validation under its integrity/fingerprint contract, but no serialized provenance bit creates authority the standalone artifact lacks.
- **Boundary-local transients.** Any compiler-only proof or workspace that is not representable and independently validatable in the receiving artifact is consumed before the persistent phase boundary; no parallel table restores it later.

Mechanical starting points:

```text
grep -rnEi 'compiler.generated|internal|privileg|reserved|visibility|private' kio-rs/src/
grep -rnEi 'internal|privileg|compiler.generated' specs/prime.md specs/grammar.md ai/topics/implementation.md
```

For each candidate, perform the fresh-load counterfactual: construct or emit the same textual artifact through the least-trusted supported entry point, then confirm the receiving validator independently proves the claimed edge. A spelling that simply disables visibility, a module-wide capability standing in for one selected member, or a cache/dynamic-load flag trusted without revalidation is a finding.

## 9. Compile-time evaluator inputs

Compile-time evaluator entry points must preserve phase-boundary purity. Audit every caller that feeds the evaluator behind `equiv`, the REPL `:normalize` command, user elaborators, generated-term templates, and future `__comptime__` hooks.

Check:

- The reducer input is a Kio'-shaped core term plus an explicit primitive environment. In kio-rs this is `UncheckedPrime` while compile-time holes are still explicit and `Prime` after final validation.
- Primitive behavior, including future `__comptime__` behavior, is represented in that environment rather than as ambient access to builtin modules, compiler internals, or hidden global state.
- A boundary adapter may consume typed Lowered terms plus `Elaborations` only before reduction begins, and only by writing the known elaborations into the Kio'-shaped evaluator input.
- The reducer and its caches do not consume Lowered or surface AST nodes, `Elaborations`, `TypeCtx`, resolver tables, or any other typer-only state after evaluator input construction.
- The same reduction semantics are shared across the entry points; wrappers may prepare terms, configure primitives, compare results, or reify diagnostics, but must not fork a second evaluator with different redex coverage.
- The full `kio` path's substituted Prime artifact is validated before emission, caching, or downstream use. `kio-prime` checking standalone Kio' input does not by itself validate Prime produced by the full Kio substitution path.

Mechanical starting points:

```
grep -rnE 'normalization|normalize|Elaborations|typecheck_full|__comptime__|template' kio-rs/src/
grep -rnE 'eval\(|evaluate\(|normalize' kio-rs/src/
```

Report any reducer/cache path over a `Lowered` AST plus `Elaborations` as a phase-boundary finding. A boundary-local `Elaborations` table is acceptable only when an adapter consumes it by writing the elaborated Kio'-shaped structure into `UncheckedPrime` / `Prime` before reduction begins.

## 10. `structural_recovery` consumes Kio' shape only

Read `kio-rs/src/pass/structural_recovery.rs`. The pass's input is `Module<Prime>` and its job is to lift right-leaning intrinsic chains into the enriched IR. Every recovery decision must be driven by:

- **Intrinsic call structure** — `__pair__`, `__fst__`, `__snd__`, `__left__`, `__right__`, `__either__`, `__if_then_else__`.
- **Type-arg shapes** read via `expr_to_type_arg` / `type_arg_type` — these are part of the Kio' call's argument vector.
- **Kio'-identity names** — an item's `name`, a `TypeMember.name`, a sum-arm's structural discriminator.

Recovery must **not** read:

- `Newtype.from_labels_label` or any other surface-provenance back-reference. At this post-label-elaboration boundary, recover ordinary newtype field structure from resolved nominal identity, payload and member calls. `ffi_key()` supplies a lowered host key, never a Surface label binding; pre-erasure Surface transformations and materializers still use explicit label declarations and relations.
- `*.ret_elided` or other "was this elided in surface" flags.
- Any field that encodes "the surface looked like X" rather than "the Kio' shape is Y."

Mechanical check:

```
grep -rn 'from_labels_label\|ret_elided\|type_alias_name' kio-rs/src/pass/structural_recovery.rs
```

Each hit is a finding: the pass is consulting surface provenance, which couples the recovery to the front-end's history rather than to the Kio' input it claims to accept. Generalize the grep with any provenance fields you identified in step 7.

For each cheat, sketch the Kio'-only recipe that would replace it (e.g., "recover field-like shape from generated newtype constructor/projector calls rather than from a surface-provenance marker"). If no Kio'-only recipe exists, the cheat is a symptom that Prime is missing information it actually needs — which is a spec-level finding, not a code-level one.

## How to report

Group findings into:

1. **Prime extensions** — syntax, identifier classes, AST inhabitants, privileges, or meanings accepted by Kio' but not by Kio with the same meaning.
2. **Forms admitted into Prime** — surface-only variants that aren't `Infallible` in the Prime phase, or that `prime::lower` accepts silently.
3. **Missing elaborations** — dispatch positions that don't record an elaboration, or recorded elaborations the substitute pass doesn't consume.
4. **Feature leakage** — kio-only modules reachable from the kio-prime build.
5. **Surface-provenance survival** — fields on Prime-surviving nodes that exist only as back-references to surface forms; for each, name the surface form, the downstream consumer, and whether a Kio'-shape recipe exists.
6. **Evaluator boundary leaks** — compile-time reducer/cache paths that consume Lowered/surface ASTs, `Elaborations`, hidden typer state, unvalidated substituted Prime, or implicit primitive state instead of an explicit Kio'-shaped evaluator input plus an explicit primitive environment.
7. **Producer-authority leaks** — reserved/compiler-generated spellings, parser modes, cache fields, or dynamic-load flags that grant authority the receiving phase cannot rederive, especially a broad private capability standing in for one exact selected edge.
8. **Recovery cheats** — places where `structural_recovery` (or any other post-Prime pass) reads a surface-provenance field instead of computing from Kio' shape.

For each finding, cite file/line and explain which surface form (or surface-only information) is leaking and at which boundary.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

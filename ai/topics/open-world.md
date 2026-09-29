# Open-world compilation — pitfalls and design checklist

Trigger: designing a feature that touches name resolution, imports, dispatch, or type-argument inference.

The headline rule lives in AGENTS.md § Universal rules. This page is the working catalogue of patterns that *look* reasonable but break open-world monotonicity, plus the checklist a feature design should clear before it ships.

**Scope: module bodies, not contract surfaces.** Open-world is a property of `*.kio` *module bodies* — the formal theorem ([`specs/formal/prime.md` § 7](../../specs/formal/prime.md)) is scoped there. It does **not** extend to a package's **contract surface**: the package-file `bridge` block and the env / export surfaces it derives from the bridged modules. Changing a contract surface is a **versioning event**, not an open-world violation. See [`specs/package.md`](../../specs/package.md) for the package-file contract. Don't write prose that implies open-world covers package bridge contracts; it doesn't.

When proposing a language feature, a resolution rule, an import semantic, anything that involves looking things up across module boundaries, **explicitly check** that adding a new declaration to a module body cannot change the meaning of existing consumer code.

## Common pitfalls

Patterns that look reasonable but violate open-world:

- **"Exactly one match in source" rules** — e.g., an import that resolves to "the unique X exported by module M." Tomorrow M exports a second X (in a separate keyspace, or with a different shape) and yesterday's unambiguous import becomes ambiguous.
- **"Longest match" or "most specific match" rules** where the candidate pool can grow — same failure mode.
- **Implicit-bringing across module boundaries** — e.g., importing a function automatically brings any operator binding tied to it. Adding a new `pub op` in the imported module changes parsing in every importer of the function.
- **Inference from context** where the context can be enriched — e.g., type-arg inference that consults all in-scope candidates, where adding a candidate changes which one is picked.
- **Retained inference backed by an ambient declaration pool** — delaying a
  nested call is safe only when its eventual inputs come from the same finite
  written call tree. Resuming it by searching newly visible functions, types,
  implementations, or role declarations makes yesterday's call sensitive to
  tomorrow's declaration.
- **Glob imports that depend on what's exported** — fine for *enabling* new names, but never use them as the basis for a *resolution choice* that affects existing code.

## The fix

Make resolution **structurally deterministic** — keyed by identity that doesn't depend on what else exists. Either:

- The consumer types enough to make the resolution unambiguous regardless of the source's content (e.g., a shape-explicit import pattern that pins exactly one (shape, key) tuple).
- The resolution is keyed by a structural identity that uniquely identifies one item by construction — fully-qualified path, op-token sequence + shape + keyspace, etc. — independent of the source's full export set.

When you sketch a feature, **state the open-world argument explicitly** in the design. If you can't, you haven't proven the design preserves open-world and the design is unfinished.

For application-local inference, the admissible domain is the connected
written call tree: resolved callee types, explicit arguments, the enclosing
expected type, and the lexical literal-role pool whose contract-surface
coupling is specified by the language. Declaration signatures, parameter-group
spelling, and named-callee provenance do not survive as parallel call-selection
evidence. A nested call may share that finite domain while its result remains
unfinished; an independently completed nested call contributes only its closed
result.
Neither case may inspect an ambient declaration set. A type-producing
elaborator likewise uses the one already-resolved implementation and the one
checked term it returns, never an ambient declaration search. Adding an
unrelated module-body declaration therefore cannot change which goals exist,
the order in which retained actions are consumed, or the result.

Flat application boundaries use the same finite evidence. Once a callee is
resolved, written argument and nested-application boundaries determine the
product packet greedily; an argument's inferred type cannot trigger a suffix
search or alternate packing. Returned binders may use the local expected
result only after a written value layer has completed. Neither rule consults
what other declarations happen to be in scope.

# Formal semantics of Kio' and the Kio → Kio' boundary

This directory holds the formal companion to the prose specs in [`../`](../). Most pages here pin Kio'-level meta-theory (typing, reduction, equivalence); [`elaboration.md`](elaboration.md) is the page whose subject is the **surface-to-core boundary** — how a Kio surface program becomes a Kio' tree. The prose pages remain the contract; the pages here pin down the meta-theory the prose claims, as inference rules and proof sketches. The per-form semantics and soundness arguments of the reference coercion-elaborator libraries (the algebraic and spine palettes, `match!`, `derive!`) are not language meta-theory — they are documented as case studies of the executable proof-of-concept package those libraries live in; see [`../../docs/poc/elab.md`](../../docs/poc/elab.md) and [`../../docs/poc/optics.md`](../../docs/poc/optics.md).

The level of formality is "rigorous enough that a careful PLT reader can re-derive everything." The pages are not mechanized in a proof assistant — existence of a written-down argument is the bar.

## Pages

| Page | Status | Subject |
|---|---|---|
| [`prime.md`](prime.md) | **Foundational.** Read first. | Kio' typing, reduction, type safety, strong normalization, decidability. |
| [`elaboration.md`](elaboration.md) | Builds on `prime.md`. | The bidirectional elaborator from Kio surface to Kio': two modes ⇑ / ⇓, source-bounded exact type-argument inference, shallow skolemization, positional type-args, no let-generalization, shared callable-layer selection for ordinary and user-elaborator calls, the named-callable registry for ordinary intrinsics, and the IR-preservation contract. Builds on PJ-V-W-S 2007. |
| [`equiv.md`](equiv.md) | Builds on `prime.md`. | The reduction strategy and equivalence relation `kio test` uses to discharge `equiv` items. |

## Reading order

1. **[`prime.md`](prime.md)** for the typing rules, reduction relation, and the meta-theorem statements.
2. **[`elaboration.md`](elaboration.md)** when you want the surface-to-Kio' bridge: how surface forms reduce to standard call shapes, how type-args are inferred at calls, why `let` neither narrows nor generalizes, and the IR-preservation contract that ties typing decisions to the later IR.
3. **[`equiv.md`](equiv.md)** when you want to know what `kio test` actually discharges.

## Pointers from informal specs

Each prose spec page that has a formal counterpart links here:

- [`../prime.md`](../prime.md) → [`prime.md`](prime.md)
- [`../language.md`](../language.md) § Type system → [`elaboration.md`](elaboration.md)
- [`../language.md`](../language.md) § Equivalence claims → [`equiv.md`](equiv.md)
- [`../cli.md`](../cli.md) § kio test → [`equiv.md`](equiv.md)

## Not formalized

The following Kio' / surface concerns have written-down prose but no formal companion. They are intentionally out of scope here; the prose specs are the contract.

- The **module system**, including the use-graph / cycle rule and the open-world property. Both are captured at the prose level only.
- The **codegen contract** for each backend (`../backends/`). Codegen is described at the value-shape and host-record level; nothing here commits to a specific operational semantics on the host side.
- **Source-level static rules** that are not type-directed (parse productions, naming conventions, `pub` visibility, `use`-resolution). These are grammar / front-end concerns; the inference-rule presentation in `prime.md` assumes a well-formed elaborated tree as input.
- **Host items** beyond their declared signatures. Host calls are opaque atoms in `prime.md` and `equiv.md`; whatever the host actually does is outside Kio's contract by design (see [`../prime.md`](../prime.md) § Host declarations).

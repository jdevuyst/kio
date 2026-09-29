# Writing and inspecting Kio (agent navigator)

This is the agent's first stop for working with the Kio language: writing surface `.kio` and reading the Kio' it compiles to. It **routes** to the existing human-facing docs and the formal specs — it does **not** re-teach them. The rich material already exists; this file makes it findable and adds the few things an agent needs that a human learning Kio does not.

Keep it that way: when this file starts *explaining* the language instead of *pointing* at where it's explained, that content belongs in `docs/guides/` (see § Improving this guide). [`audit-kio-guide`](../skills/audit-kio-guide/SKILL.md) enforces the discipline.

The lowering-discipline rule (surface forms must not survive into Kio') lives in its own topic: [`ai/topics/surface-forms.md`](surface-forms.md). Read it whenever a `.kio` edit introduces a surface form.

## Writing surface Kio — public docs first

Use [`docs/README.md`](../../docs/README.md) as the public guide catalog. It lists the language tutorial, tooling tutorial, topic guides, and host integration guides in one place, so this agent-only navigator does not need to mirror every authoring topic. Use `specs/` for authoritative contracts when the docs and specs differ.

For canonical declaration and configuration punctuation, see [`specs/style.md` § Semicolon clause blocks](../../specs/style.md#semicolon-clause-blocks).

For Kio' lowering discipline, keep [`ai/topics/surface-forms.md`](surface-forms.md) one click away. For changes touching name resolution, imports, dispatch, or inference, read [`ai/topics/open-world.md`](open-world.md) and state the open-world argument explicitly.

## Agent deltas — what the guides don't cover for you

Eleven things a human writing Kio rarely needs but you do:

1. **The lowering mental model.** Surface Kio → Lowered → Kio'. Every non-Kio' surface form (imported elaborator calls such as `if!`, `scope!`, `do!`, and `match!`, plus tuple literals, placeholder lambdas, `labels`, `op`) is removed before the Kio' boundary — [`ai/topics/surface-forms.md`](surface-forms.md). Holding this model is what makes type errors and the Kio' dump (§ below) legible: when surface behaviour surprises you, the Kio' is the unambiguous truth.
2. **A fn signature is ordered groups.** Write the canonical form `fn foo[*F][A](m: Monad(F), x: F(A))`: the `[*F]` / `[A]` type-binder groups sit in source order alongside value groups. Source may use the shorthand `fn foo[*F, A](…)`, but `kio fmt` prints adjacent singleton binders. This is the single most common shape agents get wrong. Details in [`docs/guides/higher-kinded-types.md`](../../docs/guides/higher-kinded-types.md).
3. **Test-harness and corpus conventions.** A golden is a cross-implementation language, diagnostic, or runtime case with `workdir/`, `expected.{stdout,exit}`, one stderr policy file, and exactly one execution contract: `run.args` for the harness-owned test+build+runner path, `run.test-only` for a library, or `run.sh` only for nonstandard language/CLI work. Golden-owned code treats generated host-backend files as opaque: it may pass an output directory to a fixed runner protocol, but never reads, copies, greps, patches, imports, or native-compiles those files. Put an independently authored public host-interface compile or a durable generated-artifact assertion under [`test-data/emissions/`](../../test-data/emissions/README.md); put private compiler invariants in unit or mutation tests. Kio' remains directly inspectable when the phase artifact or dynamic-load boundary is the case's subject. Prefer `run.args` or `run.test-only` over a golden `run.sh`; standard `run.args` cases run `kio test` before build/run unless they carry `IS_KIO_PRIME`, and laws go in `equiv` blocks. See [`docs/guides/equiv.md`](../../docs/guides/equiv.md), [`TESTING.md`](../../TESTING.md), and [`test-data/README.md`](../../test-data/README.md).
4. **Package skeleton.** A package is a `<pkg>.pkg.kio` (host + bridges + optional `build` block) plus module files (`module x;`) and any root `*.kio` files. Package-file shape is in [`docs/guides/pkg.md`](../../docs/guides/pkg.md).
5. **Prefer qualified imports in agent-written modules.** For ordinary modules, including root modules, default to the qualified `import path/to/module as p;` form and call through the alias (`p.name(…)`). Use selective imports when the bare name is part of the syntax or the subject being exercised: elaborator/operator names, names deliberately treated as the module's local API, and focused tests/docs for selective import behavior. `import __intrinsics__;` and `import __comptime__;` are builtin imports without alias forms. Host declarations follow ordinary import visibility. The import shapes are specified in [`specs/language.md` § Module system](../../specs/language.md#module-system); the default here is a readability convention for new Kio written by agents.
6. **Use `pub` only for intentional exports.** Definitions are private by default; add `pub` only when another module, a bridge surface, a POC adoption surface, or the focused test/doc subject is meant to import the item. Keep implementation helpers, local constructors, and intermediate test scaffolding private. Visibility is specified in [`specs/language.md` § Module system](../../specs/language.md#module-system).
7. **Use `&` for product types; commas are list/signature syntax.** Write `(A & B) -> R`, not `(A, B) -> R`. A declaration signature may still use comma-separated value parameters (`fn f(a: A, b: B) -> R`), and calls/tuple literals still use commas; those surfaces fold to right-associated product values. The right-fold call rule is in [`specs/language.md` § Type parameters](../../specs/language.md#type-parameters); `()` as the zero-component product is in [`specs/language.md` § Anonymous sum and product types](../../specs/language.md#anonymous-sum-and-product-types).
8. **Stay on surface forms for ordinary Kio.** Avoid `import __intrinsics__;` in normal program, docs, golden, and castle source. Reach for tuple syntax, labels/newtypes, `match!`, `widen_sum!`, row access/update, and `rec(loop)` first. If that feels impossible, treat it as an authoring-guidance gap or a sign that the example needs named domain branches. Raw intrinsics belong in Kio' reference material, compiler-lowering tests, intrinsic-focused goldens, and debugging sessions where the lowered core itself is the subject.
9. **Reuse the abstraction's owner, not a local clone.** Follow [`test-data/poc/README.md` § General-purpose abstraction ownership](../../test-data/poc/README.md#general-purpose-abstraction-ownership): one POC owns each general-purpose abstraction and its literal syntax. Use its declared dependency when a public API needs the shared nominal identity; otherwise use ordinary shapes, private domain-specific state, or a fold/visitor/callback seam. The dedicated Option owner does not justify local named Options for routine `A | .` absence. Do not replace per-element-type cons lists with another public generic List owner.
10. **Re-verify a claimed toolchain limitation before inheriting its workaround.** When existing Kio justifies an awkward shape with a comment claiming a limitation — an elaborator said not to tolerate an import shape, a function value said to need an eta-wrapping lambda instead of being passed bare — reproduce the failure with a minimal `kio check` probe before writing the same shape. Limitations dissolve as the toolchain evolves and the justifying comments go stale; when the probe passes, fix the stale pattern and its comment ([`ai/topics/comments.md`](comments.md)) rather than spreading it.
11. **Use `pure` only on ordinary functions.** An `elab` declaration is never marked `pure`; its implementation always runs purely. Mark a named implementation and every ordinary helper it calls `pure fn`. A same-module implementation may remain private; a cross-module implementation needs enough visibility to be imported into the elaborator's defining module, while private helpers in the target's own module remain ordinary implementation details. Captures are quoted runtime dependencies rather than compile-time calls, so they may name host items and must be at least as visible as the elaborator. Local type annotations may mention any well-formed type; named types in declaration signatures must also meet the declaration's visibility floor. See [`docs/guides/elaborators.md`](../../docs/guides/elaborators.md) and [`specs/language.md` § Module system](../../specs/language.md#module-system).

## Inspecting the Kio' your code lowers to

The public guide is [`docs/guides/debugging-with-kio-prime.md`](../../docs/guides/debugging-with-kio-prime.md); this section is the quick reference for agents already inside a `.kio` task.

Kio' is the **typechecked truth**. When surface behaviour surprises you — an emitter result you can't explain, an elaborator that elaborated differently than you expected — compile to Kio' and read it. It's unambiguous and post-typecheck, so it tells you what your code *actually means*, not what you intended.

This section is for reading the specified backend-neutral Kio' phase artifact. Do not use it as precedent for golden-owned inspection of generated host-language backend files, and do not translate the table below back into source-authoring style; use the surface forms in § Agent deltas when writing Kio.

**There is no dump command — Kio' is a build target.** Add a `kio-prime` target to the package's `build` block, then build it:

```kio
// in <pkg>.pkg.kio
build {
  target kio-prime {
    out "out/kio-prime"
  }
}
```

```sh
kio build kio-prime      # writes one Kio'-text .kio file per module under out/kio-prime/
```

`kio build kio-prime` runs the standalone Kio' validator before emitting valid Kio' **source** (not an AST dump) — one `.kio` per module. Read `out/kio-prime/<module>.kio`.

**Reading key — surface form → what you'll see in Kio'** (`specs/prime.md` § What's in Kio' / § What's not in Kio'):

| Surface | In Kio' |
| --- | --- |
| Elaborator `!`-calls (`iso!`, `into!`, `one_prod!`, `widen_sum!`, …) | **gone** — replaced by explicit intrinsic trees |
| Tuple literal `(a, b)`; product projection | `__pair__(a, b)`; `__fst__` / `__snd__` |
| Sum injection / elimination | `__left__` / `__right__`; `__either__` |
| imported `if!` with `else` block | elaborated branch-selection code using `__if_then_else__` |
| imported `match!` block | an `__either__` / `__fst__` / `__snd__` decision tree with let-bound clauses |
| imported `do! bind` block | nested `bind(…)` chain |
| imported `scope!` block | ordinary scoped sequence ending in its final expression |
| `!`-elimination | `__absurd__` |
| `.stem. { … }` placeholder lambda | explicit `.(…)` with generated parameter names |
| `labels` | generated `newtype`s; `op` → underlying `fn` calls; `literal` declarations eliminated; `equiv` stripped |
| `(A & B) -> R`; `A & B`; `A \| B` | right-folded single-param arrows; binary `&` / `\|` (surface chains nest) |

The eight intrinsics enter scope via `import __intrinsics__;`. Full semantics: `specs/prime.md` § Value construction and elimination, § Branching intrinsic, § Bottom-elimination intrinsic, § The `newtype` primitive.

## Improving this guide

When you struggle, hit the key insight that unblocks you, and realise the instructions should have told you — here's the protocol. It mirrors how goldens work: nothing lands unverified.

1. **Is it guide-worthy?** Non-obvious (it cost you real time), reusable (not specific to one task), and about the *language or workflow*. **Not a compiler bug.** If the insight is really "the emitter chokes on X, so write Y instead," that's a bug to surface and fix (AGENTS.md § Universal rules — Bugs surface; never hide them) — never an idiom to teach around. Reject those here.
2. **Verify before writing.** It goes in only if backed by a **spec citation** or a **working golden/example**. "I think X" → no. "X works, here's the golden" or "the spec says X, but I couldn't find the guide" → yes.
3. **Route it to the right home:**
   - The guidance *existed but was undiscoverable* → sharpen the pointer in this file. (Most common — the content was in `docs/guides/`, it just wasn't findable. Fix the routing, don't add prose.)
   - A genuinely missing **language idiom** → a concise entry in the relevant `docs/guides/` page (single source; humans benefit too).
   - A **workflow** insight → the § Agent deltas section here.
   - A real **spec gap** → fix `specs/` (it's a contract; inspection *tricks* don't belong there — those stay here).
4. **Write it as part of your change**, *if* verified; the change is reviewed like any other before it lands. If you're unsure, **surface it in your report** instead of writing it — unverified guidance is worse than none.
5. **Keep it context-free** (no-leak): state the fact, not "an agent struggled with X." See [`ai/topics/no-leak.md`](no-leak.md).
6. **The audit backstops you.** [`audit-kio-guide`](../skills/audit-kio-guide/SKILL.md) checks that every pointer here resolves and that this file keeps routing rather than re-teaching.

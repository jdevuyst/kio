---
name: audit-open-world
description: Scan for resolution patterns that break open-world monotonicity — adding declarations to a module body must never change the meaning of existing downstream code
allowed-tools: Read, Grep, Glob, Bash
---

# Open-world audit

Open-world compilation is a hard rule (see AGENTS.md § Universal rules and `specs/language.md` § Open-world design; the pitfall catalogue lives at [`ai/topics/open-world.md`](../../topics/open-world.md)). Adding new declarations to a module body must never cause a different module to fail to compile or change its meaning. This skill scans for patterns that quietly violate that property.

Read [`ai/topics/open-world.md`](../../topics/open-world.md) and `specs/language.md` § Open-world design before starting so the rule and the listed pitfalls are fresh.

## 1. Suspect language in prose

Grep `specs/`, `docs/`, `ROADMAP.md`, `README.md`, and `kio-rs/` doc-comments for resolution-shape phrasing that is hard to make open-world:

- "longest match" / "longest prefix" / "most specific"
- "unique X" / "the only" / "the single"
- "exactly one match" / "exactly one candidate"
- "search the package" / "find the X with"
- "glob" near "resolve" / "import" / "lookup"
- "auto" / "implicit" / "automatic" near "import" / "bring" / "expose"

For each hit, read the surrounding paragraph and decide whether the design states an explicit open-world argument. If it doesn't, flag it — even if you suspect the design is actually fine, the missing argument is itself the finding.

## 2. Resolution paths in the implementation

Read the kio-rs name-resolution and lookup code (`kio-rs/src/**/resolve*.rs`, `name_resolution*`, `package*`, `import*`, anything involved in mapping a path-segment to a declared item) and check that each lookup is **structurally deterministic**:

- Lookup is keyed by an identity that uniquely identifies one item by construction (fully-qualified path, op-token sequence + shape + keyspace, etc.) — *not* by "best match among everything in scope."
- If a resolution consults a candidate pool, adding a new candidate cannot change the result for an unchanged consumer.
- Type-argument inference does not pick differently when a new candidate appears (see `synth_call`, `apply_*` in `typecheck_core`).
- Glob imports may introduce new names but must not be the basis for a resolution *choice* over existing code.

Flag any path where the contrary isn't obvious from the code.

Discharged pattern — multi-reading resolution over the parser-validated module map: a "try reading A, else reading B" lookup whose readings disagree on how many trailing segments name the item (fn-in-module vs newtype-member) is safe when the readings are **name-role-disjoint** — module path segments match the exact value-name grammar while type names match the exact type-name grammar (specs/language.md § Naming conventions), so the segment that would have to be both a module leaf and a newtype name can never make both readings viable, and first-match ordering cannot be hijacked by a new declaration. This discharge holds only for resolvers over the parser-validated module map; the dyn_load_prime runtime loader consumes hand-written Kio' image text whose module names are *not* role-validated, so its one-dot lookups face a genuine fn-vs-member collision and resolve it by an explicit documented fn-first preference (its ambiguity reporting covers same-kind duplicates) — a resolver-local policy, not this role-disjointness discharge.

## 3. Recent feature additions

Run `git log --since="3 months ago" -- specs/ kio-rs/src/` and look at commits that introduced features touching resolution, dispatch, inference, or import. For each:

- Does the commit message or the spec section it lands state the open-world argument explicitly?
- If not, derive it yourself and flag the missing argument.

The AGENTS.md rule is: "When sketching a feature that touches name resolution, imports, dispatch, or inference, state the open-world argument explicitly." A landed feature with no recorded argument is a finding regardless of whether the feature is actually fine.

## 4. Scope check

Open-world covers *module bodies*, not the package-file contract surface (host declarations and bridge declarations). If you find prose claiming open-world over the package-file boundary, that's also a finding — the scope is deliberate, see the **Scope: module bodies, not contract surfaces** paragraph in [`ai/topics/open-world.md`](../../topics/open-world.md).

## How to report

Group findings into:

1. **Likely violations** — a specific lookup or design that can change meaning when another module body grows.
2. **Missing arguments** — designs whose open-world story is not stated, even if probably fine.
3. **Scope creep** — prose that conflates module-body open-world with the contract surface.

For each finding, cite the file/line and quote the suspicious phrasing or sketch the violating scenario (what an upstream addition would break).

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md).

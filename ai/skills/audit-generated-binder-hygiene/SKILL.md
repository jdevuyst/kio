---
name: audit-generated-binder-hygiene
description: Verify compiler passes, Kio' emission, and host backends keep generated value, type, and import bindings capture-proof through reserved identities, stable public semantic mappings, or namespace-complete private allocation.
allowed-tools: Read, Grep, Glob, Bash
---

# Generated binding hygiene audit

[`ai/topics/implementation.md`](../../topics/implementation.md) § Generated
binding hygiene is the anchor contract. This audit inventories every compiler-
or backend-generated **value, type, and import binding** and proves that users
cannot forge a colliding identity. It covers front-end rewrites, checked-Kio'
normalization, recovery passes, Kio' source emission, and every local or
item-level host binding emitted by each backend.

This is not a prefix grep. A counter, source span, hash, or unusual-looking
prefix can make collisions unlikely without making capture impossible. The
audit starts from binding sites, traces generated names to all bound uses,
and requires semantic collision evidence.

Read `AGENTS.md`, the anchor section, and
[`ai/topics/emit.md`](../../topics/emit.md) § Disciplines before starting.

## 1. Inventory from binding sites

Walk the places that **introduce** a value, type, or import binding, then trace
each identity backward to determine whether it came from source or was
synthesized:

- Kio AST/IR `let` binders, value parameters, lambda parameters, symbolic
  case payloads, pattern/row-let temporaries, continuation parameters, and
  wrapper parameters;
- Kio AST/IR `forall` binders and other type parameters, generated aliases or
  nominal owners, recursive-type wrapper names, and generated import aliases
  or qualifiers;
- rewrites that move a binder across a lexical boundary or reconstruct a
  value or type binding after Kio' checking, including substitution beneath
  `forall`;
- the Kio' backend's emitted value, type, and import declarations at every
  scope;
- host-language package/module-level helper functions and values, wrapper
  methods, statics/constants, namespace/object value members, and generated
  closure/function declarations;
- host-language generic parameters, type aliases, nominal type declarations,
  generated wrapper/shape owners, and imports or import aliases;
- declarations emitted inside package functions, conversion helpers, match
  arms, closures, and runtime scaffolds.

For each generated family, record:

1. construction site and lexical owner;
2. declaration site and every reference site;
3. reserved spelling, stable public semantic mapping, or private allocator;
4. proof for that route: user-unreachability, public injectivity and occupancy
   independence, or why no reserved class exists and allocation is necessary;
5. collision proof;
6. focused tests that distinguish capture from correct binding.

Use text searches only to seed the walk. Useful starting points include
helpers or fields containing `fresh`, `temp`, `local`, `param`, `bind`,
`recv`, `rhs`, `payload`, `slot`, `type_param`, `alias`, `import`, `generic`,
or `counter`, plus `format!` calls whose result is assigned to an AST
`name`/`param` field or emitted next to a host declaration or import. Also
inspect helpers that return a `String` consumed later as a binding; a
literal-prefix search alone misses those indirections.

Do not classify these as generated bindings unless they introduce an identity
in a value, type, module, package, or other consuming namespace:

- filesystem temporary names and cache keys;
- diagnostic-only labels or residual atoms;
- data property/member labels and structural-shape keys used only for lookup;
  methods, callable members, type members, namespace/object value slots, and
  statics remain in scope because they introduce bindings;
- package identities, source module paths, and shape-registry keys that are
  not emitted as declarations or aliases; an emitted namespace/module owner or
  import alias remains in scope.

Do not conflate namespaces while applying those exclusions. A host type
parameter remains in scope even when host values cannot collide with it, and a
value-only seed does not discharge an import-introduced type or module binding.
Classify each imported identity into the namespace that actually resolves it;
merge namespace sets only where that representation's resolution rules merge
them.

## 2. Discharge every generated family

A generated binding passes through exactly one of these routes. A private
implementation binding uses the reserved route whenever the consuming
representation provides one; collision allocation is a constrained fallback,
not an interchangeable style choice. A host-visible facade identity follows
the stable public semantic-mapping route instead of occupancy.

### Syntactically reserved

Prove that the identity is unavailable at every user declaration site that
can share its namespace. For a Kio binding, cite the grammar rule and parser
rejection. For an emitted host binding, prove that no legal Kio identifier can
reach the spelling through that backend's identifier mapping and that the
spelling is legal in the host language.

A comment saying "reserved," a conventional leading underscore, or a prefix
that users merely tend not to write is not proof. Check the parser and the
mapping code.

### Stable public semantic mapping

Trace each public facade identity from its semantic package/module/declaration
coordinate through a deterministic injective renderer under the host's name
equivalence. The mapping is independent of declaration occupancy: adding an
unrelated declaration cannot rename an existing public selector. Prove the
mapping with host-compiling facade evidence, including semantic coordinates
whose readable candidate spellings collide.

### Collision-allocated

First establish why neither the textual artifact's grammar nor its identifier
mapping can provide a usable identity outside the relevant user-reachable
namespace. A backend controls its mapping: show why escaping or mangling all
user-derived names cannot reserve a generated-binding class there. An
allocator used only because ordinary identifiers are convenient is a finding.

Prove that the allocator is seeded from the complete relevant namespace of the
owner before choosing a name:

- a value seed includes value parameters, lets, nested lambda/case binders,
  path heads that a rewrite can bring into local position, prior generated
  values, and any free value newly enclosed when a rewrite widens scope;
- a type seed includes in-scope `forall` and other type parameters, type-path
  heads, alias and nominal owners, prior generated type names, and free type
  names newly enclosed or introduced by substitution;
- for an identity introduced by an import, the actual value, type, module,
  package, or other consuming-namespace seed includes every authored,
  imported, and generated identity visible at its use sites, including aliases
  and qualifiers where those are what the consuming representation binds;
- an emitted-host seed additionally includes every user-derived and fixed
  runtime/helper identity introduced in that same host namespace, including
  generated alias/nominal owners and backend type parameters.

An allocator for a private implementation binding must be deterministic within
the lexical owner and independent of declarations outside that owner. A new
colliding binding inside the owner may force a different private spelling;
declaration/use identity and program meaning must remain unchanged. A
monotonically increasing counter is sufficient only after candidate membership
is checked against the complete namespace-specific set.

All comparisons use the consuming representation's identifier equivalence
after every relevant transformation. For a host backend, that includes
escaping or raw-identifier decoding, case/Unicode normalization where
applicable, truncation, and mangling. Distinct raw emitted strings that the
host resolves to one identity still collide. The reserved-class proof and
allocator set both operate on these canonical identities.

## 3. Trace capture and alpha-renaming

For every rewrite that moves or renames a binding:

- confirm the declaration and all of its bound references use the same
  identity;
- confirm shadowed references belonging to an inner or outer binder are not
  renamed with it;
- confirm a binder widened over a continuation is renamed when that
  continuation contains a free colliding name;
- confirm sibling and nested generated scopes cannot reuse a name in a way
  that changes binding;
- state the open-world argument: a declaration outside the lexical owner
  cannot perturb a private generated spelling, while a same-owner collision
  may change only that private spelling without changing binding or program
  meaning; a public facade identity is stable under unrelated declaration
  growth rather than occupancy-allocated.

For type substitution, explicitly inspect every `forall` descent. If a
replacement has the binder name free, the binder and all of its bound
occurrences must be alpha-freshened before substitution; an occurrence used as
the head of an applied type is still bound and must be renamed. Trace generated
alias and nominal owners through every type path that names them, and trace an
import-introduced identity through every qualified or unqualified use, as the
consuming representation resolves it. Do the equivalent declaration/use walk
for backend generic parameters.

Treat a generated binding that survives a Kio' dump as ordinary source on
reload. The emitted spelling must remain capture-safe when reparsed without
access to the original compiler's hidden state.

## 4. Require semantic evidence

Prefix assertions and "the counter changed" tests are discovery evidence,
not a hygiene proof. Each allocator family needs focused evidence that would
fail on capture:

- an implementation-level test deliberately occupies the allocator's first
  candidate in the same namespace and checks declaration/use identity under
  nested shadowing;
- a type-rewrite test forces a free-name collision beneath `forall`, including
  an applied-head occurrence, and distinguishes alpha-freshening from capture;
- a Kio-AST value or type binding that can reach emitted Kio' has a dump →
  fresh parse → Prime check/recovery comparison covering the collision;
- a backend-generated host value, type parameter, alias/nominal owner, or
  import-introduced identity has focused coverage that compiles the emitted
  artifact beside colliding user-derived names in the same host namespace and
  exercises it when executable; an allocator fixture occupies its first
  candidate, while a reserved-identity fixture pairs the artifact evidence
  with a mapping/parser test proving no legal binder can occupy it under host
  equivalence;
- a scope-widening rewrite tests a free colliding continuation name, not only
  an already-bound sibling.

Route that evidence by the boundary it proves:

- A natural language/runtime capture regression belongs in a cross-
  implementation golden and reaches the generated artifact only through a
  fixed ordinary runner protocol. Golden-owned code does not read, copy, grep,
  patch, import, or native-compile host-backend output.
- A public generated facade collision that an ordinary host author encounters
  belongs in a backend-first `HOST_INTERFACE` emission. Its fixed host is
  independently authored from the public backend spec and does not scrape the
  output to discover spellings.
- Exact private helper candidates, allocator seeds, and no-filesystem
  invariants belong in Rust unit, generator-self, or mutation tests. An
  `ARTIFACT_SHAPE` emission must not pin them; that marker is only for durable
  spec- or recorded-measurement-backed artifact facts.

Kio' remains the specified backend-neutral phase-artifact exception: a golden
may read or assemble it when hygiene across dump/reparse, validation, or
dynamic loading is the subject, and a harness-owned phase check may do the
same. This exception does not authorize direct host-backend artifact handling.

Do not put a compiler helper's private name into a golden merely to make the
collision fire. A natural public regression is appropriate when ordinary
domain names reproduce the capture without implementation narration.

This is a static evidence audit: read the focused tests and verify that their
assertions distinguish correct binding from capture. Do not turn the default
umbrella audit into a Cargo or corpus run. Missing or inadequate executable
evidence is a finding; ordinary CI executes the tests.

## 5. Backend completeness

Derive the backend set from `specs/backends/*.md` excluding the README. For
each backend, inspect every binding namespace: package/module helper functions
and values, wrapper methods, namespace/object value members, statics/constants,
body emission, FFI wrappers, shape conversions, match lowering, partial
application, closures, and runtime helpers; plus generic type parameters,
generated aliases and nominal owners, wrapper/shape type declarations, and
identities introduced by imports. Include any generated declaration that can
share a host namespace with a user-derived identity. Classify imports into the
actual value, type, module, package, or other namespace they populate, and
audit actual namespaces separately unless the host unifies them. A backend is
not clean merely because its generated prefix differs from the front-end's.

When reviewing a newly added backend, require the generated-binding naming plan
and adversarial collision fixtures mandated by
[`add-backend`](../add-backend/SKILL.md). Absence of either is a blocking
finding for that backend's acceptance.

## How to report

Group findings into:

1. **User-reachable private generated bindings** — a reserved spelling was
   available but not used, or the fallback lacks a demonstration that no
   reserved spelling is usable or lacks a complete collision allocator.
2. **Unstable public semantic mappings** — a host-visible facade identity is
   non-injective under host equivalence or changes when an unrelated
   declaration is added.
3. **Incomplete namespace seeds** — allocator misses a lexical or emitted
   name class, or an imported identity was assigned to the wrong actual
   consuming namespace.
4. **Capture/rename errors** — declaration and bound references diverge, or
   widening or `forall` substitution captures a free name.
5. **Round-trip gaps** — Kio' emission/reload can lose the original hygiene
   proof.
6. **Backend gaps** — an actual host binding namespace, including one populated
   by imports, lacks a plan or collision coverage.
7. **Evidence gaps** — tests assert prefixes/uniqueness but never force a
   collision under nested shadowing.

For each finding, cite the generated-name family, construction site, lexical
scope, colliding legal spelling, and missing or failing evidence.

**Default: report only.** If invoked with a fix-it directive, follow
[`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). Keep
independent allocator families in separate commits, add the focused
regression before claiming the family fixed, and never make a public golden
implementation-shaped to force a private name.

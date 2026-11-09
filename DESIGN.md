# Kio's design

Kio is a language for portable business logic and glue code: domain rules,
data transformations, and coordination of capabilities supplied by a host.
Its design combines reusable, statically typed components with a small
semantic core.

The goals and design decisions described here are project commitments.
Existing contracts remain binding while a change is considered.

This file explains priorities and rationale. The [specifications](specs/)
define accepted programs, semantics, and compatibility guarantees. Design
rationale does not authorize exceptions to those contracts.

## Composition with static guarantees

Abstractions earn their place by composing predictably with other abstractions.
A sound, decidable static type system makes that composition useful: it rejects
incompatible combinations before execution and gives users and tools contracts
they can rely on. The same structure supports principled optimization,
navigation, completion, and useful diagnostics.

The safety claim has a defined boundary. Kio's
[type-safety contract](specs/formal/prime.md#4-type-safety) concerns well-typed
Kio terms. Host implementations must fulfill their contracts; static typing
alone cannot rule out host bugs or arbitrary logical errors.

### A polymorphic type system

Kio builds on System F with higher-kinded quantification and application,
nominal existentials, sums, and products. Its higher-kinded system is a
restricted fragment of F-omega. Monadic composition and type-driven macros
add practical expressive power around that foundation. This combination is
Kio's chosen balance between expressiveness and tractable reasoning.

## Local reasoning and monotonic extension

A reader should be able to understand a component through its code, explicit
dependencies, and their public contracts. Type checking, resolution, and
extension mechanisms should preserve that ability.

Adding declarations to a module body must preserve the validity and meaning of
existing downstream programs. This [open-world guarantee](specs/language.md#open-world-design)
lets libraries grow without changing choices that consumers already made.
Changing a dependency's implementation or public contract is a separate matter;
the guarantee applies to extension of module bodies.

An accepted addition to a module's imports must preserve the validity and
meaning of its existing function bodies. Compatibility is checked against
the module's imports and declarations, including names and operator syntax,
without inspecting function bodies to see whether conflicts are used.

### Predictable type inference

Visible program structure should provide natural boundaries for local inference.
A local `let` finishes typing its initializer before later uses are considered.
When that expression can be typed independently of its enclosing use, introducing
such a binding or inlining it at its single use should preserve acceptance,
resulting types, and meaning. This assumes unchanged lexical scope and runtime
evaluation count, order, and conditionality. Moving an expression that needs
contextual type information across a boundary may require an annotation.

Making type choices that successful inference already determined explicit,
including inferred type arguments at their corresponding call stages, should
preserve acceptance, resulting types, and meaning. Macros may generate code from
determined types, but making those same choices explicit should neither change
the generated program’s meaning nor turn a successful expansion into an error.

Inference should follow one derivation rather than search alternative typings.
Constraint propagation may complete postponed work; the compiler must not retry
macro expansions until one succeeds.

### Self-contained operator syntax

[Operator imports](specs/language.md#operators) state the complete grammar, so
a module can be parsed and formatted without reading its providers. Resolution
then checks that grammar against the explicitly selected export. This keeps
syntax local without giving up library-defined operators.

### Trait-like programming through dictionaries

Kio expresses trait-like behavior through dictionaries: ordinary typed values
that bundle the operations generic code needs. A generic function receives the
chosen dictionary as an argument. This makes the selected implementation
explicit and permits different implementations for the same type.

The library-defined [`derive` macro](docs/poc/macros.md#derive--instance-deriving)
can automate dictionary construction by composing candidate functions supplied
at its call site. Together, dictionary passing and derivation provide generic
operations and instance construction associated with traits and type classes.
Keeping the candidate set explicit preserves local reasoning and open-world
extension.
The [higher-kinded-types POC](docs/poc/hkt.md) demonstrates dictionaries and
monadic composition together.

### Recorded interface compatibility

[`kio sig`](specs/versioning.md) records and checks package contract evolution.
The compatibility relation permits adding exports and removing host
requirements, while checking retained signatures and type identities.
`kio sig status` reports interface changes that still need to be recorded or
finalized.

This complements open-world compilation at the package boundary. It checks the
recorded interface contract; it does not prove behavioral equivalence of
dependency implementations. A signature-preserving change can still change
what a function computes.

## Mathematical clarity and practical ergonomics

A small set of regular, explainable rules is valuable both mathematically and
in everyday programming. Kio values soundness and decidable checking alongside
readable source and manageable annotations.
The strongest designs make the mathematical explanation and the programmer's
mental model reinforce each other.

The design balances expressive power, reasoning cost, portability, and ease of
use. Features are judged by how well they work together and by the complexity
they introduce throughout the language and its tools.

### Readable grouping

Readers should be able to determine syntactic grouping from visible structure
and a small set of regular composition rules, without knowing parser
implementation details. Explicit boundaries and
[declared chaining rules](specs/language.md#operators), such as associativity
or greediness, can make parentheses unnecessary. Where those rules leave
competing groupings, Kio requires parentheses rather than choosing by incidental
parser order or by which interpretation happens to succeed.

### An explicit core and an expressive surface

[Kio'](specs/prime.md) is the explicit semantic subset shared with full Kio.
Surface conveniences, macro calls, and the richer inference of omitted
type arguments are resolved before reaching it. Kio' retains bounded local
checking rules; the distinction concerns which reasoning belongs in the
surface elaborator and which belongs in the persistent core.

## Human authors, AI tools, and compilers

Kio is designed to be written and understood by people, generated and revised
by AI tools, and used as a compilation target. Readability, predictable rules,
and precise feedback matter across all three uses. Human ergonomics and
reliable code generation are both language-design concerns.

Generated Kio remains ordinary Kio: the same syntax, typing rules, and meaning
apply regardless of who or what produced it.

## A stable foundation

Kio should provide a small, stable language foundation. Users should be able to
build and share new abstractions through libraries, making their problems
natural to express. Library extensibility lets programming techniques and
domain vocabulary evolve without requiring a language change for each new
abstraction.

Kio's small core of syntax and reserved intrinsics supports library-defined
functions, operators, and control abstractions. Programs declare these locally
or import them explicitly; `if`, `match`, and `do` are ordinary library
macros.

### Type-driven macros

Kio's [imported macros](specs/language.md#macros-are-imported-not-ambient)
provide typed compile-time extension at direct named call sites. Explicit
`macro` import selectors identify these ordinary Kio library declarations;
normal lexical resolution selects each call. Their generated
terms must satisfy the receiving context's contract and become ordinary Kio'.
This gives users a way to build abstractions without adding a compiler
primitive for each library operation.

Macros reflect function types, not declaration signatures. Value-parameter
grouping belongs to function-body binding and host-interface generation, not
type reflection.

Compile-time evaluation is pure. Its semantic results and failures depend only
on explicit semantic inputs, never on allocation history, cache state, or
evaluator scheduling. Invalid inputs or insufficient type information may cause
evaluation to fail. Reusing a successfully computed result must be equivalent
to recomputing it from the same inputs.

## Portable business logic and glue code

Kio is designed for portable business logic and glue code. Domain rules, data
transformations, and coordination logic should be reusable across applications
written in different host languages. Kio packages are components embedded in a
host.

Portability preserves Kio's semantics across host languages. It also requires
usable host interfaces: embedding, calling, and loading a package are part of
the language's practical design.

### Explicit host capabilities

The host supplies capabilities such as numbers, strings, I/O, and iteration
through explicit typed interfaces. This makes the dependency boundary visible
and lets the same package fit different environments.

### Compile-time integration and dynamic loading

Kio supports both compile-time integration and dynamic loading, with typed
contracts at the host boundary. See the [shared backend contract](specs/backends/README.md).

## Developer tools are part of the language experience

Clear diagnostics, fast feedback, formatting, navigation, and reliable editor
support are design requirements. A feature's cost includes the work required
to explain it, inspect it, and repair mistakes involving it. The quality of
these interactions matters alongside the quality of the type system.

### Tools for the development loop

The [`kio` CLI](specs/cli.md) provides checking, building, testing, formatting,
and documentation generation. The language server brings diagnostics,
completion, navigation, and refactoring into the editor. Validated examples in
[Kiodoc](docs/guides/kiodoc.md) connect explanations to checked Kio code.

## Inspection without a host implementation

Users should be able to inspect and reason about Kio logic without first
implementing its host interface. Host capabilities can remain abstract while
users examine types, understand generated glue, and check relationships between
expressions. Abstractions should remain understandable under those conditions.

### Normalization

Core reduction strongly normalizes with host calls treated as opaque. Execution
of host capabilities, including iteration, lies outside that termination
guarantee. See the [normalization contract](specs/formal/prime.md#5-strong-normalization).

The REPL's [`:normalize` command](specs/cli.md) exposes residual normal forms.
Users can examine what an expression reduces to without implementing a host
interface or executing host capabilities.

### Equivalence

[`equiv` claims](specs/language.md#equivalence-claims-equiv) check that
expressions reduce to the same normal form under shared binders. The check
uses the same host-independent reduction relation as normalization, with host
calls left opaque. Its conclusions depend on Kio's semantics rather than a
particular host implementation.

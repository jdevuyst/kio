# Kio terminology

This reference explains terms specific to Kio, and familiar terms whose use in
Kio can be surprising. Follow the links for teaching examples and precise rules.
The [specifications](specs/) define behavior;
[DESIGN.md (online)](https://github.com/jdevuyst/kio/blob/main/DESIGN.md) defines
design commitments. This glossary is the vocabulary reference, not an additional
behavioral contract.

## Data shapes and types

**Product, tuple; sum, alternative.** A product type `A & B` carries two
components; a tuple such as `(a, b)` is a product value. A sum type `A | B`
carries one selected branch and its payload. Branches are positional:
`A | A` still has two distinguishable alternatives, just as `A & A` has two
components. The reference also calls products *conjunctions* and sums
*disjunctions*. These names do not give them intersection-type or Boolean
equality rules. Prefer *sum* to an unqualified *union*, which can suggest that
duplicate branches collapse. See [products](docs/guides/products.md),
[sums](docs/guides/sums.md), and the
[type rules](specs/language.md#anonymous-sum-and-product-types).

**Spine.** The right-hand chain of a product or sum tree. `A & B & C` means
`A & (B & C)`; its spine has three components. `(A & B) & C` has a different
tree, with the grouped product in its first slot. Order and grouping matter to
ordinary type equality. Library [spine macros](docs/poc/macros.md#two-source-to-target-palettes)
perform explicit structural transformations.

**Row-typed record, row, field.** Row-typed records use ordinary products of
nominal field wrappers. A declaration such as `type { Name: String; }`
introduces the field type `Name`, whose `mk` and `get` members construct and
project its payload. A *row* describes this field-oriented use of product
structure, rather than a separate type kind or runtime container. An empty row
is `()`; a one-field row can have type `Name` alone. `Name & .` retains a
second, Unit component. In `Name & R`, the row tail `R` is an ordinary type
variable: this is Kio's *row polymorphism*. It does not introduce unordered
type equality. Library `record` and `with` macros require a target and can
reorder an exact product to it; `record_` and `with_` derive their result order
from their inputs. `select` applies explicitly supplied functions to matching
fields. Other rearrangements require explicit operations. Nominal
types can also distinguish sum alternatives. See [rows and records](docs/guides/products.md)
and [named sum branches](docs/guides/sums.md#when-branches-need-names).

**Nominal type, constructor, projector.** A nominal entry in a `type` declaration
introduces a distinct type identity around a payload. Its `mk` member wraps a
payload and its `get` member projects it. An existential projector passes its
hidden payload type to a continuation so it cannot escape its scope. A
transparent `type` alias names an existing type without creating a nominal
identity. A *type constructor* instead takes type arguments to produce a type:
`Box[A]` names a type, while `Box.mk[A](value)` constructs a value. Kinds
describe the accepted type arguments, including higher-kinded parameters. See
[declarations](docs/guides/declarations.md),
[higher-kinded types](docs/guides/higher-kinded-types.md), and
[existentials](specs/language.md#existential-type-binders).

**Type equality versus conversion.** The Unit type `.` has the sole value
`()`, and bottom `!` has no values. `A & .` and `A | !` are distinct from `A`
under ordinary type equality. A macro can generate a conversion between
these shapes when its library contract admits it. The same distinction applies
to reordering, reassociation, and other isomorphisms: an available conversion
does not make two types equal. See the
[structural equality rules](specs/language.md#anonymous-sum-and-product-types).

**Call stage; type application; value application.** A singleton type group `f[A]`
instantiates one exposed universal binder. `Map[K][V]` supplies types to a
constructor; `42[I32]` or `count[I32]` annotates a literal or literal alias.
Each type or binder bracket contains exactly one entry; comma lists and empty
brackets are rejected. A value group `f(a)` applies one
function layer; `f(a,b)` and `f((a,b))` supply the same product argument.
Successive function layers require successive value groups, as in
`f[A](a)[B](b)`. Omitting a type group retains local type-argument inference.
See
[call stages](specs/language.md#bidirectional-elaboration) and the
[language tutorial](docs/tutorials/language.md#calls-have-stages).

## Checking and extension

**Kio, Kio', surface, core.** Kio is the full language. Kio' (pronounced
"Kio prime") is its explicit semantic subset, also called the *core*.
Surface forms such as tuples, compact type declarations and macro calls are translated
to that shared subset. A file's Kio' syntax, a compiler AST's Prime shape,
successful type validation, and successful dynamic loading are different
claims. In particular, the
[dynamic loader](docs/guides/dynamic-loading.md)
validates structural and host/export contract edges, trusting precompiled
function-body typing. See [Kio'](specs/prime.md).

**Elaboration, compiler elaborator; macro, macro call.**
Elaboration is the compiler's process of resolving surface code and type
information into explicit core code; the *compiler elaborator* performs it.
A *macro* is a library-defined type-driven metaprogram declared with `macro`. Its
implementation runs while a written macro call, such as `match(value) { clauses }`,
is checked. Written nonempty prefix arguments use ordinary list parentheses; a
call with no written prefix, such as `scope { body }`, omits the list. UFCS
can supply a receiver without a written prefix list.
It works with reflected types and checked-term recipes and produces checked
core code for that call site, rather than substituting source text.
Macros are declared or imported
explicitly, with ordinary resolution; their names do not make them compiler
primitives. Reflected terms describe code, rather than its computed runtime
value. The shared normalizer reduces core terms and executes prepared pure
macro implementations during elaboration; the surrounding typechecking resumes
after the replacement is checked. `:normalize` displays a residual normal form,
and `equiv` compares normal forms under shared binders. See [defining macros](docs/guides/macros.md) and
[building macros](docs/guides/macro-authoring.md).

**Builtin module, intrinsic, library.** Builtin modules expose the compiler's
specified primitive interfaces. Intrinsics are reserved operations in
`__intrinsics__`; `__comptime__` supplies the compile-time reflection interface.
Ordinary libraries, including those defining `if`, `match` and `do`, use
these interfaces without becoming builtins themselves. See
[builtin modules](docs/guides/builtin-modules.md).

**Operator word, slot, atom, frame.** A fixed operator alternates whole symbol
words with value slots: `_` accepts an atom, `__` also its own operator's
chain, and `___` also another operator's chain. A dotted slot (`._`, `.__`, `.___`)
wraps its operand in a thunk without changing that parsing permission. An *atom* is a syntactically
bounded expression, including a fixed operator completed by a final symbol
word; a pattern ending in an operand needs declared chain permission or
grouping to compose in that position. A variadic *frame* is a bare or marked
square pair, such as `[]` or `[* *]`. See [operators](docs/guides/operators.md),
[collection frames](docs/guides/variadic-operators.md), and the
[grouping contract](specs/language.md#operators).

**Normalization, residual, equivalence.** Normalization reduces core code
without running arbitrary host capabilities. A *residual* is what remains
after that simplification; REPL displays need not be reparsable Kio' programs.
An `equiv` claim compares the specified normal forms under shared parameters,
not every possible behavior of host implementations. See
[equivalence](docs/guides/equiv.md) and the [REPL](docs/guides/repl.md).

**Pure function.** `pure fn` restricts executable declaration references in a
function body. It does not classify types or promise that every call is
effect-free: a supplied callback may have effects. Macro implementations
execute under the pure-function rules. See
[function purity](specs/language.md#module-system).

**Recursive data versus recursive computation.** Recursive data uses explicit
nominal boundaries. Recursive computation uses `rec(loop)`: the written path
resolves to an ordinary function whose public type must have the required loop
shape. A host-supplied loop can execute iteration.
Core normalization's termination guarantee leaves host execution outside its
boundary. See [recursion](docs/guides/recursion.md).

## Modules and hosting

**Package, module, component.** A package is the hosted compilation and
distribution unit described by a `.pkg.kio` file. Modules provide named
declaration scopes within it. *Component* describes the package's architectural
role inside an application; it is not another declaration kind. A configured
dependency name, module path, and emitted artifact namespace serve different
purposes and need not have the same spelling. See
[packages](docs/guides/pkg.md) and [package files](specs/package.md).

**Host, host program; host language, runtime.** The host is the embedding
program or code that supplies capabilities and invokes package exports.
The *host language* is the language used for that integration. *Runtime* may
mean its execution engine, generated support code, or the interpreter used for
dynamically loaded Kio'; name the intended one when context is insufficient.
See [hosting](specs/language.md#embeddability-and-portability) and
[host guides](docs/hosts/).

**Capability, host declaration.** A capability is an operation or facility
made available to code, including ordinary function values and dictionary
bundles passed as arguments. Host capabilities, such as strings or I/O, enter
through typed `host type` and `host fn` declarations. See
[dependency injection](docs/guides/dependency-injection.md).

**Literal category, role, carrier.** The four scalar literal categories are
integer, float, Boolean and string. A host type's `role(...)` specifies which
literals it admits, including numeric bounds or IEEE rounding. Different roles
can belong to one category: `i32` and `i64` are both integer roles. The carrier
is the exact type selected by an explicit literal annotation or concrete local
context. A role supplies neither operations nor an implicit candidate for an
unresolved literal. See [literal typing](docs/guides/typechecking.md#literal-types-and-local-inference)
and [roles](specs/package.md#roles).

**Public export, host export, bridge, env, contract surface.** `pub` controls
importability between modules. A package's `bridge` selects modules whose
public callables become host-invocable exports, whose public types contribute
the type surface, and whose public host declarations become host requirements,
also called the *env*. The *contract surface* includes those
exports, requirements, and their reachable type identities. Backend-specific
APIs present this contract through native interfaces and bindings. "Host
interface" is contextual: it can name a backend's host-supplied interface or
the broader package boundary. Use *host requirements*, *exports*, or *contract
surface* when that distinction matters. See the [bridge](specs/package.md#the-bridge-block)
and [contract surface](specs/package.md#compatibility).

**Open-world compilation.** Adding declarations to a module body preserves
the validity and meaning of existing downstream code. Changes to the package's
contract surface, including added host requirements, are versioning events
outside this guarantee. Changing an existing declaration or dependency
implementation is also a separate operation. See the
[open-world contract](specs/language.md#open-world-design).

## Builds and environments

**Target language.** The language of emitted code. This describes the output
side; *host language* describes integration. A backend and an embedding host
need not correspond one-to-one. Kio' output is a phase artifact rather than
host-language integration. See [backends](specs/backends/README.md).

**Backend, backend ID.** The compiler's emitter and its integration contract,
selected by an identifier such as `js` or `rust`. The `kio-prime` backend emits
Kio'. A backend is distinct from a named package build configuration.

**Build target, target name.** A package configuration selecting a backend,
output location and options. For example, `target (js, "web")` and
`target (js, "worker")` select the same backend with different target names;
`kio build web` selects the first configuration. Qualify *target language* and
*build target* where bare *target* would be ambiguous. See
[build target files](specs/package.md#build-target-files).

**Toolchain, platform.** A target-language toolchain is the set of compilers,
interpreters, runtimes, type checkers, libraries or SDKs needed to check or run
generated code. It can contain several tools. An exact installed tool version
is distinct from a backend's documented minimum language/runtime requirement.
A *platform* identifies an execution environment such as an operating system
and architecture; it is not a package build-target name. See
[installation (online)](https://github.com/jdevuyst/kio/blob/main/INSTALL.md) and each
[backend contract](specs/backends/).

**Generation baseline.** A named backend contract for the generated code's
language/runtime requirements and host API, selected by a target's `baseline`
field. For example, Rust's `v1_85` baseline includes edition 2024 and the
Rust 1.85 minimum; it does not select an exact installed compiler. Concrete
baselines stay selected across Kio upgrades, while `latest` follows the table
in that Kio build. Formatting materializes omitted or Unit selections into
concrete names. See [baseline selection](specs/package.md#per-target-keys).

## Tools and compatibility

**Check, build, test.** `kio check` checks source; `kio build` transpiles a
package for selected build targets; `kio test` checks `equiv` claims. Running
generated code requires its host. The REPL inspects modules and normalizes
expressions without providing a general host environment. See
[tooling](docs/tutorials/tooling.md) and the [CLI](specs/cli.md).

**Signature.** A declaration header, a callable's type, and a package signature
history are different uses of this word. A `.sig.kio` file records package
contract evolution for `kio sig`; it is not merely a function-type listing.
See [package versioning](specs/versioning.md).

**Package compatibility, Host API stability, release version.** Package
compatibility compares recorded requirements and exports. A backend's
`Host API stability: evolving|stable` field governs compatibility of its native
integration API. The Kio release version identifies the compiler distribution;
it is distinct from a package's own recorded version and exact toolchain pins.
See [package compatibility](specs/versioning.md#the-compatibility-relation)
and [Host API stability](specs/backends/README.md#host-api-stability).

**Kiodoc, manual.** Kiodoc validates and renders supported Kio fences in
Markdown. `kio manual` reads version-matched original documentation bundled
with the executable, including this glossary. See
[Kiodoc](docs/guides/kiodoc.md) and the [offline manual](docs/guides/manual.md).

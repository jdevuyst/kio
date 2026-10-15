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
ordinary type equality. Library [spine elaborators](docs/poc/elab.md#two-source-to-target-palettes)
perform explicit structural transformations.

**Row, record, field, label.** Record-shaped values use ordinary products of
nominal field wrappers. A `labels` declaration introduces lowercase label
syntax, such as `{name = value}`, and its generated nominal type `Name`.
The label selects a field; `Name` identifies its wrapper type; field access
returns the payload. A *row* describes this field-oriented use of product
structure, rather than a separate type kind or runtime container. An empty row
is `()`; a one-field row can have type `Name` alone. `Name & .` retains a
second, Unit component. In `Name & R`, the row tail `R` is an ordinary type
variable: this is Kio's *row polymorphism*. It does not introduce unordered
type equality. Construction and update have a specific
[expected-order checking rule](specs/language.md#labels); other rearrangements
require explicit operations. Labels also give sum alternatives distinct nominal
payload types. See [rows and records](docs/guides/products.md) and
[named sum branches](docs/guides/sums.md#when-branches-need-names).

**Nominal type, newtype, constructor, projector.** A `newtype` declaration
introduces a distinct type identity around a payload. Its declared value
constructors wrap payloads, and its projectors expose them through the declared
interface. An existential projector passes its hidden payload type to a
continuation so it cannot escape its scope. A transparent `type` alias names
an existing type without creating a nominal identity. A *type constructor*
instead takes type arguments to produce a type: `Box(A)` names a type, while
`Box.mk_box(A, value)` constructs a value when `mk_box` is the declared member.
Kinds describe the accepted type arguments, including higher-kinded parameters.
See [declarations](docs/guides/declarations.md),
[higher-kinded types](docs/guides/higher-kinded-types.md), and
[existentials](specs/language.md#existential-type-binders).

**Type equality versus conversion.** The Unit type `.` has the sole value
`()`, and bottom `!` has no values. `A & .` and `A | !` are distinct from `A`
under ordinary type equality. An elaborator can generate a conversion between
these shapes when its library contract admits it. The same distinction applies
to reordering, reassociation, and other isomorphisms: an available conversion
does not make two types equal. See the
[structural equality rules](specs/language.md#anonymous-sum-and-product-types).

## Checking and extension

**Kio, Kio', surface, core.** Kio is the full language. Kio' (pronounced
"Kio prime") is its explicit semantic subset, also called the *core*.
Surface forms such as tuples, label operations and bang calls are translated
to that shared subset. A file's Kio' syntax, a compiler AST's Prime shape,
successful type validation, and successful dynamic loading are different
claims. In particular, the
[dynamic loader](docs/guides/dynamic-loading.md)
validates structural and host/export contract edges, trusting precompiled
function-body typing. See [Kio'](specs/prime.md).

**Elaboration, elaborator, bang call.** Elaboration is the compiler's process
of resolving surface code and type information into explicit core code.
A *library elaborator* is an ordinary resolved `elab` declaration whose
implementation runs while a written bang call, such as `match!`, is checked.
It works with reflected types and checked-term recipes and produces checked
core code for that call site. "Typed compile-time macro" explains its purpose;
it does not mean textual substitution. Elaborators are declared or imported
explicitly, with ordinary resolution; their names do not make them compiler
primitives. Reflected terms describe code, rather than its computed runtime
value. See [defining elaborators](docs/guides/elaborators.md) and
[building elaborators](docs/guides/elaborator-authoring.md).

**Builtin module, intrinsic, library.** Builtin modules expose the compiler's
specified primitive interfaces. Intrinsics are reserved operations in
`__intrinsics__`; `__comptime__` supplies the compile-time reflection interface.
Ordinary libraries, including those defining `if!`, `match!` and `do!`, use
these interfaces without becoming builtins themselves. See
[builtin modules](docs/guides/builtin-modules.md).

**Normalization, residual, equivalence.** Normalization reduces core code
without running arbitrary host capabilities. A *residual* is what remains
after that simplification; REPL displays need not be reparsable Kio' programs.
An `equiv` claim compares the specified normal forms under shared parameters,
not every possible behavior of host implementations. See
[equivalence](docs/guides/equiv.md) and the [REPL](docs/guides/repl.md).

**Pure function.** `pure fn` restricts executable declaration references in a
function body. It does not classify types or promise that every call is
effect-free: a supplied callback may have effects. Elaborator implementations
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

**Capability, host declaration, role.** A capability is an operation or facility
made available to code, including ordinary function values and dictionary
bundles passed as arguments. Host capabilities, such as strings or I/O, enter
through typed `host type` and `host fn` declarations. A type's `role(...)`
associates it with specified literal or
Boolean interpretation; it does not supply operations or an ambient standard
library. See [dependency injection](docs/guides/dependency-injection.md) and
[roles](specs/package.md#roles).

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

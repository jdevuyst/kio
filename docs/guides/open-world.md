# The open-world story

Imagine that importing a module implicitly brought every exported name into
local scope. A consumer might begin like this:

```text
# Hypothetical pseudocode — this is not valid Kio.
bring every name from cats
bring every name from dogs

speak()
```

If `cats` is the only provider with a `speak` function, the call appears
unambiguous. If `dogs` later adds its own `speak`, the unchanged consumer is
suddenly ambiguous. A declaration added somewhere else has broken code that
already compiled.

Kio forbids that kind of import-all rule. A module either selects the names it
wants from one named provider:

<!--kio {file}
module cats;

pub fn speak() -> . { () }
-->

<!--kio {file}
module dogs;

pub fn speak() -> . { () }
-->

<!--kio {harness=selective_import file placeholder="__SNIPPET__"}
module examples/selective;

__SNIPPET__
-->

```kio {@selective_import}
import cats(speak);

fn announce() -> . { speak() }
```

or keeps providers qualified:

<!--kio {harness=qualified_import file placeholder="__SNIPPET__"}
module examples/qualified;

__SNIPPET__
-->

```kio {@qualified_import}
import cats as cat;
import dogs as dog;

fn announce_both() -> . { cat.speak(); dog.speak() }
```

For names from another module, Kio only considers the ones this module imports
explicitly. Adding another declaration elsewhere cannot change what an
existing name refers to. The same separation applies to operator bindings:
importing a function does not silently import syntax associated with it. A
consumer that wants both names imports both, for example
`import list(push, op _ :: __);` when the provider declares
that complete operator grammar.

Each ordinary imported name or qualified module alias is introduced once.
Repeating the same import is an error, as is declaring a local type alias with
an already imported name. To re-export a type, import its provider under a
module alias and declare the type alias without also selectively importing it.

This is the source-level meaning of **open-world compilation**: adding a new
declaration to a module body cannot make a different, previously valid module
fail to compile or change its meaning. A consumer can opt into a new
declaration by editing its own `import` list, but a provider cannot silently
retarget the consumer's existing code.

## Explicit evidence, not ambient instance search

The same problem appears in languages that search all visible typeclass
instances. Suppose an existing call asks for an implementation and the
language searches globally:

```text
# Hypothetical pseudocode — this is not valid Kio.
formatter = find visible Formatter(Unit)
```

Adding a newly visible instance could change the selected implementation or
turn one match into an ambiguity. The problem is the ambient search, not
dictionary values or typeclass-like APIs themselves.

Kio represents such evidence as ordinary values and passes it through ordinary
function parameters. When the `derive!` library elaborator is convenient, it
examines exactly the literal tuple of rule functions supplied at that call
site:

<!--kio {harness=explicit_evidence placeholder="__SNIPPET__"}
bridge {
  kiodoc;
}

module kiodoc;

import derive(derive);

host type String role(str);

newtype Formatter[A] : A -> String {
  constructor mk_formatter;
  projector format
}

__SNIPPET__
-->

```kio {@explicit_evidence}
fn apply_formatter[A](formatter: Formatter(A), value: A) -> String {
  Formatter.format(formatter)(value)
}

fn listed_text() -> String { "listed"(String) }

fn unlisted_formatter() -> Formatter(String) {
  Formatter.mk_formatter(.(_value: String) { "unlisted"(String) })
}

fn formatter_from_text(text: String) -> Formatter(String) {
  Formatter.mk_formatter(.(_value: String) { text })
}

fn choose_string_formatter() -> Formatter(String) {
  derive!((formatter_from_text, listed_text), Formatter(String))
}
```

The target requires both listed rules: `listed_text` produces the `String`
precondition that `formatter_from_text` consumes to produce
`Formatter(String)`.
The compatible `unlisted_formatter` rule can produce the requested target
directly, but it is absent from the tuple. If `derive!` searched the module,
that rule and the listed composition would make the result ambiguous. The
fence typechecks because `derive!` considers only the literal tuple and
composes its two listed rules to reach `Formatter(String)`. Adding an unrelated
declaration therefore cannot alter this derivation. See the
[higher-kinded-types case study](../poc/hkt.md#deriving-an-instance-with-derive) for a
worked instance dictionary and the
[`derive!` case study](../poc/elab.md#derive--instance-deriving) for the rule
composition contract.

## Dependency diamonds keep nominal identities distinct

A dependency diamond can carry two materialized copies of what began as the
same nominal type. Re-rooting gives those copies different module paths. For a
`Store` newtype, the consumer can therefore see both
`core/store.Store` and `widget/store.Store`.

Those names share a leaf spelling, but nominal identity is the exact
`(module, name)` pair. Neither copy silently wins, and Kio does not unify them
automatically. Without an explicit reconciliation, an API crossing from one
copy to the other needs a public conversion.

When the copies are deliberately the same boundary, the consumer can record
that fact in the dependency declaration used for materialization:

```kio {variant=dependency}
dependency widget;

source {
  path "../widget/widget.pkg.kio"
}

retype widget/store to core/store;
```

The module-wide `retype` form reconciles same-named newtypes from the source
module with their counterparts. Materialization accepts the remap only when
the type-parameter arities match and the payloads are structurally congruent;
otherwise it reports a dependency error. The decision is explicit in the
consumer's dependency file rather than selected from an ambient pool. The full
materializer contract, including the per-type form, is in
[`specs/package.md` § Dependency files](../../specs/package.md#dependency-files),
under **Retyping a dependency's newtypes**.

## The package contract is a separate boundary

The open-world guarantee is about `*.kio` module bodies. A package's
host-facing contract is the `bridge`-selected closure of host requirements,
exports, and the types their signatures reach. Changes to that boundary are
governed by compatibility versioning. An ordinary new `pub` declaration is
still an additive module-body extension, even when its bridged export also
grows the host-facing surface; adding a host requirement, removing an export,
or changing a reachable boundary type can break a host contract.

`kio sig` records and checks that separate contract surface. The
[tooling tutorial](../tutorials/tooling.md#chapter-7--seal-the-contract-surface-with-kio-sig)
shows the everyday workflow, and
[`specs/versioning.md`](../../specs/versioning.md) defines the compatibility
rules. Contract-surface versioning does not turn ambient name or instance
search into valid module semantics.

## Normative references

The exact language rule is
[`specs/language.md` § Open-world design](../../specs/language.md#open-world-design).
Its formal statement and proof sketch are
[`specs/formal/prime.md` § 7. Open-world](../../specs/formal/prime.md#7-open-world).
The examples above illustrate that contract; they do not extend it.

For the related surface forms, see [Operators](operators.md),
[Using libraries](using-libraries.md), and
[Higher-kinded types](higher-kinded-types.md).

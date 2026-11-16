# Aliases, nominal types, visibility, and purity

Kio has two ways to name a type shape. A `type` alias gives an existing type another spelling; a nominal
entry in a `type` declaration creates a distinct boundary. Literal aliases name literal tokens rather than values. Visibility
and ordinary-function purity are separate parts of a function's contract.

The examples assume host-provided `Int` and `String` types, with the `i32`
and `str` literal roles respectively. Recursive sum construction also uses
the imported `widen_sum` macro; [Using libraries](using-libraries.md#the-macro-library)
shows how to add its library and import `macros/spine_macros(macro widen_sum)`.

<!--kio {harness=declarations placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

mod kiodoc;

import macros/spine_macros {macro widen_sum}

host type Int role(i32);
host type String role(str);

__INSERT_CODE_HERE__
-->

## Type-name spelling

Type names and type parameters use capitalized `snake_case`: each word has
letters followed by optional digits, with one underscore between words. Only
the first letter is uppercase. For example, `Item1_part2` is valid, while
`Item_2`, `Item1part` and `Item__part` are not. A name may also have trailing
underscores, as in `Item1_part2__`.

Prefix exactly one underscore when a type binding may intentionally remain
unused; the first letter after that marker is still uppercase:

```kio {@declarations}
type Alias[A] = A;

type _Unused_alias[_A] = _A;

type { _Box[_B]: _B }
```

`_Box` and `_A` are type names, while `_box` is a value name. `_1Box`,
`__Box`, and `_BoxCar` are invalid names for type bindings in user source.
The marker changes no type or visibility semantics; references use the marked
spelling exactly.

## Transparent type aliases

A nullary alias names one structural type. It does not create a new type, so
the alias and its body are interchangeable:

```kio {@declarations}
type User_name = String;

fn keep_name(name: User_name) -> String { name }
```

A parameterized alias names a family of structural types:

```kio {@declarations}
type Pair[A] = A & A;

fn duplicate[A](value: A) -> Pair[A] { (value, value) }
```

An alias reference must supply exactly its declared parameter count. Here,
`Pair[Int]` is a type; bare `Pair` supplies none and is not a first-class type
constructor. For `type Either_pair[E][A] = E | A;`, both `Either_pair` and
`Either_pair[String]` are likewise type errors because they omit parameters
declared by the alias. Aliases unfold wherever the typer compares types, and an
ordinary alias cannot refer to itself. An alias may participate in a mutual
recursive group only when a sibling nominal type provides the knot; an
alias-only cycle is rejected. Use a nominal type when the name must carry identity
or tie a recursive knot.

An alias can also retain an existing nominal type's constructor and projector
namespace when it forwards every parameter to that nominal type exactly once and in
order:

```kio {@declarations}
type { Box[A]: A }

type Wrapped[A] = Box[A];

fn roundtrip[A](value: A) -> A { Wrapped.get[A](Wrapped.mk[A](value)) }
```

`Wrapped.mk` is the constructor declared by `Box`; it still produces the
`Box[A]` nominal. A partial application, reordered parameter list, structural
alias, cycle, or missing target has no member namespace. The alias must already
be in scope, and both the alias and terminal member must be visible from the
use site.

Transparent aliases retain their normalized host type's literal admission,
including through fully applied alias bodies. This adds no nominal identity
or role and does not select an unresolved literal. A nominal wrapper instead
does not inherit its payload's literal capability. See
[literal inference](typechecking.md#literal-types-and-local-inference).

## Literal aliases

A `lit` declaration stores exactly one string, number, or boolean token.
It acquires a type only where it is used:

```kio {@declarations}
lit greeting = "hello";

lit limit = 100;

fn greeting_text() -> String { greeting }

fn explicit_limit() -> Int { limit[Int] }
```

The bare `greeting` is checked against the function's `String` return type.
`limit[Int]` supplies the same annotation as `100[Int]`. A literal alias is
not a cached value or an arbitrary expression binding: expansion substitutes
the stored token at each use. Use a zero-argument `fn` when evaluation itself
is part of the abstraction.

A literal alias keeps its lexical category: `lit count = 2000;` stores an
integer token, while `lit scale = 2.0e3;` stores a float token. Its expected
type or annotation at the use does not convert one category
into the other. After expansion, an explicit literal annotation or the
concrete local context at the use site determines its carrier; the provider
contributes the token without a preselected type.

Prefix a literal alias with `pub` to import it from another module with
`import module { lit name }`. Write that import once: repeating it is a
name-resolution error, as is selecting one visible name from distinct exported
literals or from both an import and a local declaration.

## Nominal types

A nominal entry creates a type distinct from its payload and from every other
nominal type. Its members `mk` and `get` construct and project values:

```kio {@declarations}
pub type { User_id: Int }

fn roundtrip_id(value: Int) -> Int { User_id.get(User_id.mk(value)) }
```

There is no implicit coercion across this boundary. The entry inherits the
visibility of its declaration. With no entry body, both members inherit that
visibility. A written body specifies `mk` and `get` with independent visibility;
see [Visibility](#visibility).

Parameterized members accept explicit type arguments; ordinary inference can
also supply them:

```kio {@declarations}
type { Box[A]: A }

fn box_roundtrip[A](value: A) -> A { Box.get[A](Box.mk[A](value)) }
```

A named group such as `type Person = { Name: String; Age: Int; };` introduces
`Name` and `Age` and aliases their product. It does not introduce a separate
nominal `Person`. By contrast, `type { Person: Name & Age; }` introduces one
nominal wrapper around that product.

## Recursive nominal types

An ordinary nominal declaration is source ordered and cannot see its own head. Add `rec`
when its payload genuinely refers to that nominal name. The recursive
occurrence must be strictly positive:

```kio {@declarations}
rec type { List[A]: . | (A & List[A]) }

fn empty[A] -> List[A] { List.mk[A](widen_sum(())[. | (A & List[A])]) }

fn empty_later[A]() -> List[A] { empty[A] }
```

`empty[Int]` constructs a `List[Int]` with only a type argument. The optional
`empty_later` wrapper deliberately keeps a Unit argument: `empty_later[Int]`
returns a function, and `empty_later[Int](())` constructs the list. A bare
`empty` remains polymorphic; it does not construct a list until specialized.
See [Type-only values](../tutorials/language.md#type-only-values) for the
anonymous form.

The projector exposes one layer; it does not unfold the type implicitly. The
marker supplies only `List`'s own head, not every later declaration in the
module. Omitting a necessary marker reports the recursive reference and offers
an add-`rec` fix. Writing it on an acyclic nominal type reports that the marker is
unnecessary and offers to remove it.

The list's recursive tail is stored as data, which satisfies the positivity
rule. By contrast, a payload that is a function accepting the recursive type
puts that occurrence on the input side of an arrow and is rejected:

```kio {@declarations check_exit_code=16}
rec type { Bad: Bad -> . }
```

For nested function types and parameterized wrappers, the rule follows the
whole path to the recursive occurrence. See the
[precise positivity rule](../../specs/language.md#type-declarations) for those
cases.

Mutually recursive types use one bare, capability-free group. Every member
head is visible throughout the braces, while visibility remains on each
member:

```kio {@declarations}
rec {
  type Tree[A] = Branch[A];
  type { Branch[A]: . | (A & Tree[A] & Tree[A]) }
}
```

The written group must be exactly one genuine mutual cycle. Move acyclic
dependencies before their users; do not include helpers just to gain forward
scope. Transparent aliases such as `Tree` are allowed only when every cycle
crosses a nominal member such as `Branch`. A one-member recursive nominal uses `rec type { ... }`. Term-level traversal
remains explicit through a `rec(loop)` group; see [Recursion](recursion.md).

## Existential nominal types

Trailing angle-bracket binders hide payload types while keeping the nominal type's
public arity at its square-bracket parameters:

```kio {@declarations}
type { Pack[A] <Hidden>: A & Hidden }

fn pack_string(value: Int, hidden: String) -> Pack[Int] { Pack.mk[Int]((value, hidden)) }

fn first[A](pack: Pack[A]) -> A {
  Pack.get[A](pack)[_](.[Hidden](payload) { let .(value: A, _hidden: Hidden) = payload; value })
}

fn first_pattern[A](pack: Pack[A]) -> A {
  let .(<Hidden> (value: A, _hidden: Hidden)) = Pack.get[A](pack);
  value
}
```

Construction infers `Hidden` from the payload. The projector returns a
function that accepts a callback; that callback receives the payload with its
hidden type available only inside the callback. The
`let .(<Hidden> name) = ...` form opens it for the rest of the block. The
witness is fresh and cannot escape in the result type. Every declared
existential must occur in the payload, and repeated occurrences must infer the
same type.

A typed pattern can open and unpack the payload in one statement, as in
`first_pattern`. The hidden type is available in the component annotations
but still cannot escape from the function. The pattern
`(value: _, _hidden: _)` also works, while an untyped opening
`(value, hidden)` does not.

## Visibility

Declarations are private by default. `pub` makes a declaration importable by
any module and eligible for the host-facing package surface when its module is
bridged. A written nominal member body and members of a braced `rec(loop)` group make
the choice independently:

```kio {@declarations}
pub type { Token: Int { pub mk; get } }
```

Other modules can construct `Token`, but only the declaring module can use
`Token.get`.

A `rec(loop)` group's braces only group mutually recursive bodies. Every member
becomes an ordinary module function with its own visibility, so an unmodified
member remains callable by later declarations in the same module.

`pub(path)` exports only to the subtree rooted at an ancestor of the declaring
module:

```kio {variant=module}
mod internal;

pub(internal) fn helper() { () }
```

Here `internal` and modules below it may import `helper`; unrelated modules and
the host cannot. The path must be a prefix of `internal`, so scoped
visibility can narrow an export but cannot grant sideways access. `host type`
and `host fn` declarations are always public and never accept `pub(path)`.

A declaration's signature cannot expose a type that its callers cannot name.
Every named type in a function, host-function, macro, or alias signature
must therefore be at least as visible as that declaration. The check follows
type arguments and transparent aliases, so an exported alias cannot disguise a
private nominal type. Nominal entries follow the same rule.

A nominal type can still provide an opaque public boundary. Its payload is exposed
only through its constructor and projector, so each member is checked at the
intersection of the outer nominal type's visibility and that member's visibility:

```kio {@declarations}
type { Hidden: Int }

pub type { Opaque: Hidden { mk; get } }
```

`Opaque` is public, but its members and `Hidden` remain private. Making either
member public would expose `Hidden` through that member's signature and would
therefore require `Hidden` to be public too. A `pub` member written on a private
outer nominal type remains effectively private; scoped combinations similarly use
the narrower subtree.

For a plain-public outer nominal type, the member markers give callers four useful
capability shapes:

| Constructor | Projector | What outside callers can do directly |
| --- | --- | --- |
| private | private | name and pass the nominal value |
| `pub` | private | construct it |
| private | `pub` | project it |
| `pub` | `pub` | construct and project it |

These capabilities are also the ones a bridged module presents to its host.
The compiler does not add the missing operation or expose the payload merely
because the outer nominal type is public.

## Explicit, transitive purity

An ordinary module-body function is unrestricted unless marked `pure`. The
marker constrains executable references in its body, transitively:

```kio {@declarations}
type Duplicate[A] = A & A;

pub pure fn id[A](value: A) -> A { value }

pure fn duplicate[A](value: A) -> Duplicate[A] { (id(value), id(value)) }

pure fn keep_int(value: Int) -> Int { let .(kept: Int) = id(value); kept }

pure fn apply[A][B](callback: A -> B, value: A) -> B { callback(value) }
```

A pure body may use local parameters and bindings, core intrinsics and nominal
members, and ordinary functions explicitly declared `pure`. It cannot refer to
a `host fn` or an unmarked ordinary function in executable code. Each referenced
pure function is checked by the same rule, making the promise transitive without
inspecting a callee's body at each call site. An unrestricted function may use
either kind.

A passed callback can still have effects: `apply` accepts a host callback from
an unrestricted caller, just as it accepts `id`. A call to a pure function is
effect-free when evaluating its arguments and invoking any callbacks they
contain or capture are effect-free; function types carry no purity qualifier.

Function signatures and type annotations are not executable references. They
use the ordinary type rules and may mention `host type`s, aliases, nominal types, or
any other type in scope, as `keep_int` does above. Types do not themselves have
a purity classification.

`pure` applies only to an ordinary `fn`; it is rejected on `type`, `macro`, `op`, `fold`, `lit`, `equiv`, host declarations, and
`rec(loop)` groups or members. Visibility is orthogonal: `pub pure fn` is both
exported and pure, while a private `pure fn` remains module-local.

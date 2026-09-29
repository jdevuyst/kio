# Aliases, newtypes, visibility, and purity

Kio has two ways to name a type shape. A `type` declaration gives a structural
type another spelling; a `newtype` declaration creates a distinct nominal
boundary. Literal aliases name literal tokens rather than values. Visibility
and ordinary-function purity are separate parts of a function's contract.

The checked examples on this page use this small host surface:

<!--kio {harness=declarations placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(widen_sum);

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

newtype _Box[_B] : _B { constructor mk_box; projector un_box }
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

fn duplicate[A](value: A) -> Pair(A) { (value, value) }
```

An alias reference must supply exactly its declared parameter count. Here,
`Pair(Int)` is a type; bare `Pair` supplies none and is not a first-class type
constructor. For `type Either_pair[E][A] = E | A;`, both `Either_pair` and
`Either_pair(String)` are likewise type errors because they omit parameters
declared by the alias. Aliases unfold wherever the typer compares types, and an
ordinary alias cannot refer to itself. An alias may participate in a mutual
recursive group only when a sibling `newtype` provides the nominal knot; an
alias-only cycle is rejected. Use a `newtype` when the name must carry identity
or tie a recursive knot.

An alias can also retain an existing newtype's constructor and projector
namespace when it forwards every parameter to that newtype exactly once and in
order:

```kio {@declarations}
newtype Box[A] : A { pub constructor make; pub projector open }

type Wrapped[A] = Box(A);

fn roundtrip[A](value: A) -> A { Wrapped.open(A, Wrapped.make(A, value)) }
```

`Wrapped.make` is the constructor declared by `Box`; it still produces the
`Box(A)` nominal. A partial application, reordered parameter list, structural
alias, cycle, or missing target has no member namespace. The alias must already
be in scope, and both the alias and terminal member must be visible from the
use site.

## Literal aliases

A `literal` declaration stores exactly one string, number, or boolean token.
It acquires a type only where it is used:

```kio {@declarations}
literal greeting = "hello";

literal limit = 100;

fn greeting_text() -> String { greeting }

fn explicit_limit() -> Int { limit(Int) }
```

The bare `greeting` is checked against the function's `String` return type.
`limit(Int)` supplies the same annotation as `100(Int)`. A literal alias is
not a cached value or an arbitrary expression binding: expansion substitutes
the stored token at each use. Use a zero-argument `fn` when evaluation itself
is part of the abstraction.

Prefix a literal alias with `pub` to import it from another module with
`import module(name);`. Write that import once: repeating it is a
name-resolution error, as is selecting one visible name from distinct exported
literals or from both an import and a local declaration.

## Nominal newtypes

A `newtype` creates a type distinct from its payload and from every other
newtype. Its constructor and projector are explicitly named members:

```kio {@declarations}
pub newtype User_id : Int { pub constructor mk_user_id; pub projector to_int }

fn roundtrip_id(value: Int) -> Int { User_id.to_int(User_id.mk_user_id(value)) }
```

Inside a braced declaration or section, semicolons separate peers: write one
between a constructor and a projector, but not after the final member. The
parser also accepts a semicolon at either edge and `kio fmt` removes it. The
outer declaration itself ends at `}`; a redundant following semicolon is also
accepted and removed. Nonbraced declarations, such as `type` and `labels`,
keep their required terminating semicolon. Expression blocks keep their usual
meaning: a semicolon between expressions sequences them, while an edge
semicolon does not change the block value.

There is no implicit coercion across this boundary. Callers write
`User_id.mk_user_id` to enter it and `User_id.to_int` to leave it. The outer
`pub` exports the nominal type; each member has its own visibility. For callers
outside the declaring module, an exported constructor grants construction and
an exported projector grants projection. Publishing the type alone grants
neither operation.

Parameterized newtypes take explicit type arguments in their member calls:

```kio {@declarations}
newtype Box[A] : A { pub constructor mk_box; pub projector un_box }

fn box_roundtrip[A](value: A) -> A { Box.un_box(A, Box.mk_box(A, value)) }
```

## Recursive newtypes

An ordinary `newtype` is source ordered and cannot see its own head. Add `rec`
when its payload genuinely refers to that nominal name. The recursive
occurrence must be strictly positive:

```kio {@declarations}
rec newtype List[A] : . | (A & List(A)) { pub constructor mk_list; pub projector un_list }

fn empty[A]() -> List(A) { List.mk_list(A, widen_sum!((), . | (A & List(A)))) }
```

The projector exposes one layer; it does not unfold the type implicitly. The
marker supplies only `List`'s own head, not every later declaration in the
module. Omitting a necessary marker reports the recursive reference and offers
an add-`rec` fix. Writing it on an acyclic newtype reports that the marker is
unnecessary and offers to remove it.

Mutually recursive types use one bare, capability-free group. Every member
head is visible throughout the braces, while visibility remains on each
member:

```kio {@declarations}
rec {
  type Tree[A] = Branch(A);
  newtype Branch[A] : . | (A & Tree(A) & Tree(A)) { constructor mk_branch; projector un_branch }
}
```

The written group must be exactly one genuine mutual cycle. Move acyclic
dependencies before their users; do not include helpers just to gain forward
scope. Transparent aliases such as `Tree` are allowed only when every cycle
crosses a nominal member such as `Branch`. A one-member recursive newtype uses
`rec newtype`; a one-member recursive labels declaration uses `rec labels`;
there is no `rec type` form. Term-level traversal remains explicit through a
`rec(loop)` group; see [Recursion](recursion.md).

### Migrating implicit recursive declarations

Code that relied on a declaration seeing its own head adds the singleton
marker at the declaration-modifier position. Visibility stays before it:

```diff
-pub newtype List[A] : . | (A & List(A)) { pub constructor mk_list; pub projector un_list }
+pub rec newtype List[A] : . | (A & List(A)) { pub constructor mk_list; pub projector un_list }

-labels Tree = { leaf: Int } | { branch: Tree & Tree };
+rec labels Tree = { leaf: Int } | { branch: Tree & Tree };
```

Code that relied on module-wide forward visibility instead encloses exactly
the genuinely mutual component. The declarations remain the operation units;
the group supplies only their shared scope:

```diff
-type Tree[A] = Branch(A);
-newtype Branch[A] : . | (A & Tree(A) & Tree(A)) {
-  constructor mk_branch;
-  projector un_branch
-}
+rec {
+  type Tree[A] = Branch(A);
+  newtype Branch[A] : . | (A & Tree(A) & Tree(A)) {
+    constructor mk_branch;
+    projector un_branch
+  }
+}
```

The compiler and editor use these exact messages and action titles:

| Situation | Diagnostic | Automatic action, when the complete edit is proven valid |
| --- | --- | --- |
| Valid recursive singleton lacks its marker | “recursive data declaration requires `rec`” | “Add `rec` to this recursive newtype” or “Add `rec` to this recursive labels declaration” |
| Marked singleton is acyclic | “this `rec` marker is unnecessary” | “Remove unnecessary `rec`” |
| Mutual component is not grouped | “mutually recursive data declarations require `rec { ... }`” | “Fix recursive type groups” |
| Group is acyclic | “this `rec` group contains no recursive cycle” | “Fix recursive type groups” |
| Group contains unrelated or independent components | “this `rec` group contains multiple independent components” | “Fix recursive type groups” |
| Transparent aliases form an ungrounded cycle | “recursive type component has no `newtype` boundary” | no marker or grouping action |

A negative, invariant, ill-kinded, or wrong-arity recursive occurrence reports
that underlying type error and does not offer an add-marker action. This keeps
the quick fix from turning one invalid declaration into a differently invalid
one. If a file has independent recursive group and singleton mistakes, fix the
currently reported one and let the editor reanalyse the result; the next
repair then appears. Fix All combines only repairs already published for one
analysis snapshot.

## Existential newtypes

Trailing angle-bracket binders hide payload types while keeping the newtype's
public arity at its square-bracket parameters:

```kio {@declarations}
newtype Pack[A] <Hidden> : A & Hidden { pub constructor mk_pack; pub projector open_pack }

fn pack_string(value: Int, hidden: String) -> Pack(Int) { Pack.mk_pack(Int, (value, hidden)) }

fn first[A](pack: Pack(A)) -> A {
  Pack.open_pack(A, pack)(
    , _
    , .[Hidden](payload) { let .(value: A, _hidden: Hidden) = payload; value }
    )
}

fn first_pattern[A](pack: Pack(A)) -> A {
  let .(<Hidden> (value: A, _hidden: Hidden)) = Pack.open_pack(A, pack);
  value
}
```

Construction infers `Hidden` from the payload. The projector is CPS-shaped;
the `let .(<Hidden> name) = ...` form opens it for the rest of the block. The
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
bridged. Newtype members and members of a braced `rec(loop)` group make the
choice independently:

```kio {@declarations}
pub newtype Token : Int { pub constructor mk_token; projector reveal }
```

Other modules can construct `Token`, but only the declaring module can use
`Token.reveal`.

A `rec(loop)` group's braces only group mutually recursive bodies. Every member
becomes an ordinary module function with its own visibility, so an unmodified
member remains callable by later declarations in the same module.

`pub(path)` exports only to the subtree rooted at an ancestor of the declaring
module:

```kio {variant=module}
module internal;

pub(internal) fn helper() -> . { () }
```

Here `internal` and modules below it may import `helper`; unrelated modules and
the host cannot. The path must be a prefix of `internal`, so scoped
visibility can narrow an export but cannot grant sideways access. `host type`
and `host fn` declarations are always public and never accept `pub(path)`.

A declaration's signature cannot expose a type that its callers cannot name.
Every named type in a function, host-function, elaborator, or alias signature
must therefore be at least as visible as that declaration. The check follows
type arguments and transparent aliases, so an exported alias cannot disguise a
private nominal type. Declarations generated by `labels` follow the same rule.

A newtype can still provide an opaque public boundary. Its payload is exposed
only through its constructor and projector, so each member is checked at the
intersection of the outer newtype's visibility and that member's visibility:

```kio {@declarations}
newtype Hidden : Int { constructor mk_hidden; projector un_hidden }

pub newtype Opaque : Hidden { constructor mk_opaque; projector un_opaque }
```

`Opaque` is public, but its members and `Hidden` remain private. Making either
member public would expose `Hidden` through that member's signature and would
therefore require `Hidden` to be public too. A `pub` member written on a private
outer newtype remains effectively private; scoped combinations similarly use
the narrower subtree.

For a plain-public outer newtype, the member markers give callers four useful
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

pure fn duplicate[A](value: A) -> Duplicate(A) { (id(value), id(value)) }

pure fn keep_int(value: Int) -> Int { let .(kept: Int) = id(value); kept }
```

A pure body may use local parameters and bindings, core intrinsics and newtype
members, and ordinary functions explicitly declared `pure`. It cannot execute
a `host fn` or an unmarked ordinary function. Because each called pure function
is checked by the same rule, the promise is transitive without inspecting a
callee's body at each call site. An unrestricted function may use either kind.

Function signatures and type annotations are not executable references. They
use the ordinary type rules and may mention `host type`s, aliases, newtypes, or
any other type in scope, as `keep_int` does above. Types do not themselves have
a purity classification.

`pure` applies only to an ordinary `fn`; it is rejected on `type`, `newtype`,
`labels`, `elab`, `op`, `fold`, `literal`, `equiv`, host declarations, and
`rec(loop)` groups or members. Visibility is orthogonal: `pub pure fn` is both
exported and pure, while a private `pure fn` remains module-local.

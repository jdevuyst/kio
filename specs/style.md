# Kio canonical style

This document specifies the canonical style produced by `kio fmt`. Kio's formatting is **opinionated and non-configurable**: there is one canonical style, and `kio fmt` produces it. Re-formatting already-canonical source is a no-op.

The reference implementation is the `kio` binary. Alternative implementations are expected to produce byte-identical output for the same input. Where this document under-specifies a detail (the long tail of trivia placement, numeric-literal canonicalization, string-literal escapes), `kio`'s output is normative and the golden corpus pins it.

## Determinism

Any conformant implementation produces **byte-identical output** for any input, across machines, OS versions, and locales. Hazards an implementation must guard against:

- HashMap or other unordered iteration in any layout decision.
- Locale-sensitive string comparison or sorting.
- Filesystem walk order in directory-mode `kio fmt` runs.

A divergence between two conformant implementations on the same input is a spec violation in one of them.

## Indent and blank lines

- **Indent.** Two spaces. No tabs anywhere.
- **Top-level item separation.** Exactly one blank line between top-level items in a module or package file. No blank line before the first or after the last.
- **`import` blocks.** See [§ `import` block ordering](#import-block-ordering) below — separated by exactly one blank line.

## Block bodies (`{ … }`)

Named and anonymous function bodies use a **width-driven** block layout. This includes the anonymous functions supplied as `match!` clauses. The body stays inline if the whole construct fits in the 100-column budget:

```kio
fn id[A](x: A) -> A { x }
```

If the construct overflows the budget — or the body itself contains an unconditional break (e.g., a nonempty block elaborator call or a comma-separated list that broke to A1) — the surrounding `{ … }` breaks too: the opener `{` stays on the head's line, the body sits at +2 indent on its own line(s), and the closer `}` returns to the surrounding column.

```kio
fn longer_name() -> . {
  call_with_long_name(arg_alpha, arg_beta, arg_gamma, arg_delta)
}
```

**Block elaborator calls.** Every nonempty trailing block breaks: its entries sit at +2 indentation, separated by semicolons, and its closer returns to the surrounding column. An empty block is `{}`, preceded by one space. This layout applies to every elaborator head, without consulting its declaration or the block's exposure kind. A nonempty block consequently breaks any enclosing function body or comma-separated list.

```kio
if! cond {
  then_branch
} else {
  else_branch
}
```

**Final labelled-block elision.** When the final labelled block contains exactly one complete block elaborator call, its wrapper braces are omitted if doing so preserves all comments. This is structural, not a special rule for the names `if` or `else`:

```kio
if! a {
  x
} else if! b {
  y
} else {
  z
}
```

The nested call owns all following labelled blocks. A non-final outer block keeps its braces, as does a block containing several expressions, a local binding, a non-block-call expression, or wrapper-attached comments. For example, the braces in `outer! x { a() } fallback { inner! y { b() } } cleanup { c() }` remain necessary: `cleanup` belongs to `outer`.

**Block prefixes.** The formatter prefers a bare unary prefix when the printed expression is unambiguous, such as `if! condition { … }` or `outer! convert!(value) { … }`. Multiple prefix values use `form!(a, b) { … }`; one product uses `form!((a, b)) { … }`. One unit value is `form!() { … }`, distinct from zero prefix values in `form! { … }`. Brace-leading, operator-leading and complete nested block-call prefix expressions retain the parenthesized head. A signed numeric literal may be bare, as in `form! -1 { … }`.

Grouping also preserves the enclosing first-brace boundary through operator operands, including greedy slots: `outer! left + (inner! value { a() }) { b() }` cannot lose the inner expression boundary. The formatter may group the whole prefix instead when needed by its operator layout. An operator's explicit continuation token supplies its own boundary. Comments around explicit prefix parentheses remain within the prefix; they are not discarded to shorten the head.

Comments between a closing brace and a continuation label stay at that block boundary. They do not change which call owns the label. Parsing and formatting require no elaborator-provider lookup.

## Spacing

- `x: A` — one space after `:` in type annotations.
- `let .(x: A) = e;` — typed locals use an explicit parenthesized rich pattern; the annotation uses the same spacing.
- `A & B`, `A | B` — single spaces around binary type operators.
- `A -> B` — single spaces around `->`.
- `,` separators in single-line lists — `(a, b, c)`, never `(a,b,c)` or `(a , b , c)`.

Trailing whitespace at end of line is dropped.

## Declaration modifiers

When an ordinary module-body `fn` carries both `pub` and `pure`, `kio fmt` emits `pub pure` in that order, regardless of the source order. No other declaration admits `pure`. A scope restriction stays bound to `pub` with no inner spaces — `pub(a/b)` — and precedes `pure`: `kio fmt` emits `pub(a/b) pure fn f(...)`. Every singleton recursive modifier follows visibility: `pub rec newtype`, `pub rec labels`, and `pub rec(loop) fn`. A one-member braced `rec(loop)` function group formats to its singleton shorthand for private, scoped, and public members alike. A capability-free type group always keeps its bare `rec { ... }` braces; it contains at least two members and has no singleton shorthand or group-level visibility.

**Recursive type groups.** `rec {` ends the introducing line. Each complete `type`, `newtype`, or surface `labels` member starts on a new line at +2 indentation, preserving member order; the closing `}` returns to the group's column. Visibility and doc comments stay on the member. A member never repeats `rec` because the enclosing braces supply its scope.

## Comma-separated lists: A1 (width-driven, leading commas when broken)

Every comma-separated list in Kio follows the same **A1 layout**, driven by width uniformly:

- **0–1 items.** Single-line, unconditionally.
- **2+ items.** Single-line if the flat form fits within the **100-column line-length budget**; otherwise the **leading-comma multi-line** layout below.

The multi-line form puts a leading comma on every item, including the first:

```kio
fn merge_sorted_runs[A](
  , left_run: List(A)
  , right_run: List(A)
  , compare_elements: A -> A -> Bool
  , size_limit: I32
  ) -> List(A) { left_run }
```

- The opener (`(`, `<`, `{`) is the **last token on the introducing line** — same line as the function name, type name, etc.
- Items at **+2 indent** from the introducing line's base column, each prefixed with a comma and a space (including the first).
- **A multi-line item anchors at its content column.** The comma-and-space prefix acts as one +2 nesting level: an item that spans multiple lines lays out exactly as it would at the start of a line, translated to its content column (the comma column + 2). A lambda item's broken block body therefore sits at +2 from the lambda's own header and its closing `}` returns to the item's content column (the § Block bodies rule, anchored at the construct's head) — never level with the commas. A nested list item indents its own items from the item's content column the same way.
- The closer (`)`, `}`) sits on its own line at the **same +2 indent column** as the items.
- The continuation after the closer (`-> R`, `{ … }`, `;`) follows on the closer's line.

```kio
fn filter[A](keep: A -> Bool, xs: List(A)) -> List(A) {
  reverse(fold_list(
    , xs
    , nil(A, ())
    , .(carry: List(A), x: A) -> List(A) {
        if! keep(x) {
          cons(x, carry)
        } else {
          carry
        }
      }
    ))
}
```

**Cascading on unconditional breaks.** A list whose flat form fits the budget but contains an unconditional break (a wide sub-expression that already broke, a captured comment that forces multi-line, etc.) breaks too — the trigger is "doesn't fit *or* contains a forced break." A flat list can only contain other already-flat docs.

A1 applies to every comma-separated list that remains comma-separated in canonical output:

- Signature value-parameter groups (`fn`, lambdas, `equiv`; `name: T` value params and product-destructuring patterns — `(a: A, b: B)` bare or `name: (a: A, b: B)` as-pattern — in one value group; see [`language.md` § Parameter patterns](language.md#parameter-patterns)).
- `labels` arm lists.
- Selective `import` name lists (`import path(foo, bar);`).
- Product-valued expressions, including preassembled products of `match!` clauses.
- Tuple literals (`(a, b, c)`).
- Call argument lists (`f(a, b, c)`).
- Type-application argument lists at use sites (`Foo(A, B)`).
- Label construction in expression position (`{f = e1, g = e2}`).
- Label access and update postfix lists (`x.?{f, g}` / `x.!{f = e1, g = e2}`).
- Row-let statement lists (`let .({f, g as payload}) = row;`).

Type-binder groups are the exception: source may write `[A, B]`, but canonical output always prints adjacent singleton binders (`[A][B]`). This applies in signatures, function types, `type` / `newtype` heads, and the named-form `labels T[A][B] = …` head. Function-type value groups are not comma lists; product domains use `&` (`(A & B) -> C`).

An explicit label-reuse marker keeps the canonical spelling `name: _` (or `name[A]: _` with its universal header). Formatting never expands `_` back into the original payload and never treats it as a new declaration.

A standalone label forward uses `pub(scope) type {local} = {source.label};`,
with ordinary optional visibility, compact braced names, and spaces around
`=`. Comments inside the declaration stay inside its body; in particular an
interior `///` comment does not become documentation attached to the item.
An external documentation comment remains attached to the forwarding item.

For label construction and update payloads, `kio fmt` uses the shortest canonical payload spelling: `{f = f}` becomes `{f}`, `{m.f = f}` becomes `{m.f}`, and `{f = ()}` becomes `{f=}`. The same rule applies inside update lists, so `r.!{f = f}` becomes `r.!{f}` and `r.!{f = ()}` becomes `r.!{f=}`.

Inline `match!` clauses are entries of a trailing product block, separated by semicolons: `match! v { .() { 0 }; .[B](n: B) -> I32 { 1 } }`. The block uses the block-call layout above. A preassembled product of clauses, supplied as `match! v { clauses }`, keeps the ordinary comma-list layout at the product's definition.

**Single-element lists stay single-line regardless of length.** A1 says single-line at 0–1 items unconditionally; line length doesn't override.

The trade-off this policy accepts is that adding or removing an item can reflow the surrounding list when its width crosses the 100-column threshold. The win is a uniform rule: short declarations stay short, and the only thing that changes when a list grows past the budget is its layout — not the form a reader has to recognize.

## Semicolon clause blocks

Semicolon-delimited blocks use one `;` between entries and none at either
edge in canonical output. `kio fmt` emits one entry per line when the block
breaks. Admitted leading/trailing separators and repeated runs are removed;
the owning grammar determines whether runs and empty bodies are admitted.
This applies to fields and declarations as well as expressions, not to
comma-delimited lists.

An outer braced declaration or section ends at `}`; its optional redundant
suffix `;` is omitted. Nonbraced outer entries retain their terminator. Braces
inside a qualifier or right-hand side are not declaration bodies: for example,
`host type T { owned };` and `labels N = { x: . };` retain their terminators.
Inside a semicolon-delimited block, both braced and nonbraced entries use the
parent's between-entry separators.

```kio
equiv id_unit_eq {
  id_unit();
  ()
}
```

The same final-vs-non-final rule applies inside ordinary function bodies and neutral trailing blocks, including `scope! { ... }` and `do! bind { ... }`. A non-final expression keeps a following `;`; the final expression does not. An optional final separator never turns an expression into a discarded statement or changes a block's value. A final binding retains the separator required by its binding grammar.

**`newtype` member blocks.** A `newtype`'s `{ … }` body is a semicolon-clause block of exactly two members — one `constructor` and one `projector`, separated by `;`. The members are an unordered set in the source (either may be written first); `kio fmt` canonicalizes them to `constructor`-then-`projector`.

```kio
newtype Celsius : . { pub constructor mk_celsius; pub projector to_unit }
```

## Type-operator chains

`&` (product) and `|` (sum) chains in type position emit **without outer parentheses**:

```kio
type Pair[A][B] = A & B;
type Either[A][B] = A | B;
type Triple[A][B][C] = A & B & C;
```

Chains right-associate: `A & B & C` denotes `A & (B & C)`. Only **load-bearing** outer parens are written:

- **Mixing** different operators in one chain: `(A & B) | C` — without the parens the chain would parse with the *other* operator at the top.
- **Function-type child of a chain**: `(A -> B) | C` — without surrounding parens, `A -> B | C` would reparse as `A -> (B | C)` (the function returning the sum).
- **Function-type parameter list**: `(A & B) -> C` — the outer parens belong to the arrow's parameter list, not to the chain. They're always written.
- **Left-leaning same-op chain**: `(A & B) & C` keeps inner parens to preserve the AST shape (distinct from the right-leaning chain `A & B & C`).

Parenthesized unary function types parse, but they are not canonical output: `(A) -> B` emits as `A -> B`.

When a chain doesn't fit in the 100-column budget, the formatter emits the **leading-operator multi-line** layout — the A1 analogue of leading-comma for lists. Each item lives on its own line at +2 indent, prefixed with `&` (or `|`) followed by a space (including the first), and any natural terminator (`;`, `,`, `)`, `{`) lands on its own line at the same +2 indent:

```kio
type Event =
  | Click_event
  | Hover_event
  | Focus_event
  ;
```

Same width-driven trigger as the comma-separated lists: single-line if the flat form fits, multi-line otherwise. The unparenthesized-chain canonical layout is admissible at every phase — both Kio and Kio' parse `A & B & C` and `(A & (B & C))` to the same right-associated AST, and `kio fmt` emits the unparenthesized-chain shape regardless of whether the source is Kio or Kio'.

## User-operator chains

User-defined `op` applications use a width-driven, all-or-nothing semantic-segment layout. The flat form preserves the pattern's ordinary single-space spelling. When it does not fit in the 100-column budget, the first segment remains at the expression's anchor and every later segment starts on its own line at +2 indent. Segments follow the pattern's operator-token boundaries: consecutive token runs stay together, an adjacent non-spliced operand stays on the same segment, and a trailing token run forms its own segment.

Identical-pattern children in recursive (`__`) and greedy (`___`) slots contribute their segments to the same layout decision. This covers right- and left-recursive binary operators, prefix and postfix operators, matched pairs, and higher-arity patterns without changing associativity. A different-pattern child is not flattened into the surrounding chain and retains any load-bearing parentheses.

```kio
let result =
  first_value
    :: second_value
    :: third_value
    :: final_value;
```

Variadic literals follow the same comma-list layout as tuples, retaining their
own OPEN and CLOSE runs for every element count. The flat form has a space
inside each delimiter (`[* a, b *]`), or one space between the delimiters when
empty (`[* *]`), keeping symbol runs separate. Zero- and one-element literals
stay flat under the ordinary list rule. A longer literal stays flat when it
fits; otherwise OPEN ends the introducing line, every element occupies a +2
line with a leading comma, and CLOSE occupies its own +2 line. Continuations
within an element align with its content after the comma. Leading, repeated
and trailing input commas collapse to this ordinary list layout.

```kio
let values =
  [!
    , first_value
    , second_value
    , third_value
    !];
```

## UFCS chains

A right-leaning UFCS chain `receiver.>f(...).>>g(...).>h(...)` is the canonical surface form for chained projector / callable application. The layout is the value-position analogue of [§ Type-operator chains](#type-operator-chains) — width-driven, all-or-nothing, with the dot-splice operator (`.>` or `.>>`) leading each broken line.

- **Single-line** if the flat form fits in the 100-column budget.
- **Multi-line** otherwise: the innermost receiver lives on its own line; each `.>callee(args)` / `.>>callee(args)` lands on its own line at +2 indent relative to the receiver's column. All segments break together or none break — partial breaks are not produced.

```kio
let r = receiver.>f(x).>>g(y).>h(z);
```

```kio
let r =
  some_long_receiver_expression
    .>first_step(arg_one, arg_two)
    .>second_step(arg_three)
    .>final_step(arg_four);
```

**Wide args within a single segment** are independent: the per-segment argument list falls back to its own A1 leading-comma layout if it overflows the line, regardless of whether the surrounding chain broke.

```kio
let r =
  receiver
    .>one(x)
    .>two(
      , arg_one
      , arg_two
      , arg_three
      )
    .>three(y);
```

**Zero-arg segments** emit without parentheses in either layout — `r.>f` / `r.>>f` are the no-existing-arg spellings, and a `.>f` or `.>>f` segment inside a broken chain keeps the same form.

Written empty UFCS tails are not formatter inputs: `r.>f()`, `r.>>f()`, `f().<r`, and `f().<<r` are parse errors. To pass Unit in addition to the receiver, write it explicitly as `(())`; the formatter preserves that nonempty list.

**Left-call splices** preserve their spelling. With no existing arguments, `f.<x` and `f.<<x` emit without an empty `()`; with existing arguments, `f(a).<x` and `f(a).<<x` keep the call on the left. If the inserted right-hand value is compound enough that dropping grouping would change the parse, `kio fmt` emits parentheses, for example `f.<(x.>g)`.

**Elaborator-form preservation.** Blockless elaborator calls can use the standalone prefix form (`iso!(r, T)`, `fit!(r, T)`, `checked!(r, T)`) or a dot-splice form that builds the same argument list (`r.>iso!(T)`, `r.>>fit!`, `iso!(T).<<r`, `r.>checked!(T)`). `kio fmt` preserves that user-authored spelling; it does not rewrite between prefix and dot-splice forms. Right-callee forms with no written arguments remain argless (`r.>iso!`), and nonempty argument lists keep their parentheses (`r.>iso!(T)`). Block calls instead use the direct `name! … { … }` form and the canonical block-prefix rules above; they have no UFCS form.

**User-defined elaborator declarations.** An elaborator item writes the declared call type after a colon and the implementation in a body `impl` field:

```kio
pub elab checked : [Source] Source -> [Target] Target { impl checked_impl }
pub elab identity : [Source] Source -> [Target] Target { impl identity_impl }
pub elab matching : [Source] Source -> [Target] Target { impl(fills) matching_impl }
```

The call type is rendered by the ordinary type printer. Mixed value/type-binder function types keep the ordinary prefix-binder function-type spelling (`A -> [B] B -> A`); there is no separate elaborator-signature syntax.

Declaration entries use semicolon separators. Canonical order is `captures`, then every `trailing` descriptor in its original relative order, then `impl`. The descriptor spells `trailing product`, `trailing thunk`, or `trailing sequence`, followed by its label when present. Other declaration entries may move around the descriptors in source, but the descriptors' relative order defines the call's block order and is never sorted.

The fills marker is rendered as `impl(fills)` followed by exactly one space and
the implementation's lexical value path. Ordinary mode renders `impl` followed
by exactly one space and the same path shape. Dot-separated segments have no
surrounding spaces; slash-qualified paths and non-path expressions are not
declaration syntax.

**Recursive-call annotations.** `kio fmt` preserves the presence of each `rec` call annotation but canonicalizes their order to `poly`, `cont`, `escape`. For example, `rec(cont, poly) f(x)` emits as `rec(poly, cont) f(x)`.

## `op` patterns

An `op` declaration introduces a user operator by binding a pattern of operator-character tokens and `_` / `__` / `___` placeholders to an in-scope `fn` (see [`language.md`](language.md#operators)). The pattern and compact body fit on one line:

```kio
op _ + _ { impl add }
op _ + __ { impl add }
op _ ? _ : __ { impl if_then_else }
op _ <| __ |> { impl index }
```

Layout rules:

- **One space between pattern units.** Each placeholder slot (`_`, `__`, `___`) and each maximal operator run is separated from its neighbors by exactly one space. `_+_` is rewritten to `_ + _`, and `_<|__|>` to `_ <| __ |>`. A fused run such as `&&++` stays intact while separately written runs remain separated: `_ && ++ _` and `_ &&++ _` are distinct patterns. Fixed patterns contain neither `[` nor `]`.
- **Compact body spacing.** `op _ + _ { impl add }` is the canonical form; `op _+_{impl add;};` is rewritten to it.
- **Always single-line.** An `op` declaration emits on one line. The 100-column budget does not apply: an over-budget operator declaration stays on one line rather than breaking — operator patterns aren't comma-separated, so there's no A1-style fallback layout. The outer declaration ends at its closing body brace.

Top-level item spacing (one blank line between items per [§ Indent and blank lines](#indent-and-blank-lines)) applies to `op` like every other top-level item.

**Operator import selections** retain the complete tagged grammar. Fixed
selections render every slot kind, literal run, and significant lenient group,
with the same token spacing and necessary quotation as the declaration:
`import syntax(op _ + __, op _ ( <| _ |> ));`. A variadic selection renders
`varop`, OPEN and CLOSE, with one space between them:
`import syntax(varop [* *]);`. Formatting never shortens
these selections to a dispatch key or reads a provider to recover grammar.

## Variadic operator declarations

A `varop` declaration uses one space after its keyword and between OPEN and
CLOSE. Its compact body stays on one line,
like a fixed operator declaration. The primary clause always comes first; it
renders one of `foldl`, `foldr`, `foldl1`, or `foldr1`, followed by the step
path and then the base/seed path. An optional `finalize` clause follows it.
Dotted path segments have no surrounding spaces.

```kio
varop [* *] { foldr cons empty }
varop [[ ]] { foldl append empty }
varop [! !] { foldr1 cons singleton }
varop [% %] { foldl1 append_entry seed_entry; finalize finish }
```

## Import block ordering

A module's imports are grouped into two blocks, in fixed order, separated by
exactly one blank line:

1. `import __intrinsics__;`
2. Every other import: `import __comptime__;`, selective
   `import module/path(items);`, and qualified `import module/path as alias;`.

Within each block, statements sort lexicographically by ASCII codepoint on
their full rendered string, without locale or case folding. Within a selective
list, items sort by their full rendered spelling: ordinary names are bare,
labels retain braces, and operator selections retain their complete tags and
grammars. Empty blocks are skipped.

Selective imports always use parentheses, including for one item. The opening
parenthesis immediately follows the module path on the introducing line; the
flat form is `import module/path(First, second);`. A nonempty list breaks using
the ordinary A1 rule: one item per +2 line with a leading comma, and its closer
on a +2 line. The tag and full grammar form one operator selection; the two
delimiters of a `varop` head stay together on that line.

```kio
import module/path(
  , First
  , op _ ? _ : ___
  , second
  , varop [% %]
  );
```

Comments remain attached to their statement or selected item when sorting.
Inter-token comments follow the general conservation and relocation rules in
[Comments](#comments); none is discarded by import canonicalization.

## Named-field blocks (`build`, `source`, `resolved`)

The package-file `build { … }`, dependency `source { … }`, and dependency-lock `resolved { … }` blocks are named-field blocks. `kio fmt` renders each named field with exactly one space between the key and its value; values are not column-aligned.

The package-file `build { … }` block is canonicalized as follows (see [`grammar.md` § Package files](grammar.md#package-files) for the field set):

- **Canonical key order.** Fields emit in a fixed order regardless of the order the author wrote them, so a reordering produces no diff: `cache` first, then the optional `docs { … };` block, then the `target <id> { … }` blocks in source order. Inside `docs`: `md`, then any `support` entries in source order, then the optional `md_out`, then `html`. Inside each `target`, the universally meaningful `out` key emits first, then other keys emit in ASCII order by key.
- **Insert-missing-optional-with-`()`.** The block's **optional field** is `cache`: a build block that omits `cache` gets `cache ();` inserted (caching disabled).

  This is the opposite of fmt's treatment of elided type-arguments, where fmt **never** inserts a slot (the prefer-fuller-elision ladder in [`language.md` § Type-argument inference and the `_` placeholder](language.md#type-system) keeps `f("")` over `f(_, "")`): for the build block's fields presence is the canonical form, so fmt adds the missing field; for type-argument elision absence is the preferred form, so fmt never de-elides. The two rules concern different constructs — keep them straight.
- **Field-value canonicalization.** A field value is `()`, a string literal, a number, a boolean, or a bare word (the `BlockFieldValue` grammar). String, number, and boolean literals canonicalize the same way they do in expression position (see [§ Numeric and string literal canonicalization](#numeric-and-string-literal-canonicalization)); `()` and bare words emit verbatim.

## Package `bridge` blocks

A package file's `bridge { … }` block lists the module-path globs that select the package's host boundary (see [`grammar.md` § Package files](grammar.md#package-files)). `kio fmt` renders it as:

- **One glob per line**, each at **2-space indent**, with `;` after every non-final glob; the opening `bridge {` ends its line and the closing `}` returns to column 0.
- **Source order preserved — not sorted.** Unlike `import` blocks and dependency `rehost` / `retype` statements, which `kio fmt` sorts, bridge globs emit in the order the author wrote them; the formatter never reorders them.

```kio
bridge {
  app;
  app/**
}
```

## Dependency files (`*.dep.kio`)

A `<local>.dep.kio` dependency declaration (see [`grammar.md` § Dependency file](grammar.md#dependency-file-depkio)) is user-authored and `kio fmt` canonicalizes it. The whole file is fixed: the `dependency <local>;` header, exactly one blank line, then the `source { … }` block with its origin lines at +2 indent, and a trailing newline. A local `path` source emits one `path "<rel>"` line:

```
dependency <local>;

source {
  path "<rel>/<name>.pkg.kio"
}
```

A `git` source emits `git`, `ref`, then its optional `path`, in that fixed order:

```
dependency <local>;

source {
  git "<url>";
  ref "<rev>";
  path "<repository-relative>/<name>.pkg.kio"
}
```

Without a selector, `path` is omitted and `ref` is the final field, without a semicolon. Comments preceding fields stay attached to their fields when reordered. Each value is a string literal and canonicalizes the same way string literals do in expression position (see [§ Numeric and string literal canonicalization](#numeric-and-string-literal-canonicalization)).

After the `source` block, any `rehost <from> to <to>;` statements emit — each on its own line, separated from `source` by exactly one blank line — in canonical order: sorted **lexicographically by ASCII codepoint** on the rendered `from` path, then on `to`, the same ordering rule applied to `import` imports above. Source order is not significant; `kio fmt` reorders out-of-order statements rather than erroring. Both paths render as `/`-separated module paths with no trailing item:

```
dependency <local>;

source {
  path "<rel>/<name>.pkg.kio"
}

rehost <local>/<mod> to <consumer>/<mod>;
```

## Dependency lock files (`*.lock.kio`)

A `<local>.lock.kio` lock file (see [`grammar.md` § Dependency lock file](grammar.md#dependency-lock-file-lockkio)) pins a remote `git` dependency's resolved commit and its contract-surface digest. It is **written by `kio build` / `kio dep`** and committed to version control, and `kio fmt` canonicalizes it like a `.dep.kio`: the `lock <local>;` header, one blank line, then the `resolved { … }` block with its `git` / `ref` / optional `path` / `commit` / `sig` lines at +2 indent in that order, and a trailing newline:

```
lock <local>;

resolved {
  git "<url>";
  ref "<rev>";
  path "<repository-relative>/<name>.pkg.kio";
  commit "<sha>";
  sig "<digest>"
}
```

The `path` line is omitted for a pathless Git source. Field comments move with their fields, as in dependency declarations.

## Signature files (`*.sig.kio`)

A `<pkg>.sig.kio` signature changelog (see [`versioning.md` § The `kio sig` command](versioning.md#the-kio-sig-command)) is generated by `kio sig` and canonicalized by `kio fmt` through the same signature emitter. The file begins with `signature <pkg> v(<N>);`, followed by one blank line before each version block. Version blocks emit oldest-first by version number.

Within each `v(<N>) { … }` block, the optional version doc-comment emits immediately above the `v` line as normalized `///` lines. A nonempty version-leading `with` block emits first; its module sections sort by module path, their `import` clauses use canonical import order, and declarations sort by name. Change partitions then emit in fixed order: `breaking`, then `nonbreaking`. Inside each partition, change blocks emit in fixed order: `add`, then `modify`, then `remove`, omitting empty blocks. Exact operation references sort lexicographically by their full `module/path.Item` spelling and emit before any legacy inline module sections; canonical `remove` output always uses those exact references. Module sections inside inline `add` / `modify` emit sorted by module path; `import` clauses inside a section emit in canonical import order; recorded items emit sorted by their declared contract name. All signature-file nesting uses +2 indentation, and the file ends with a trailing newline.

## Comments

Comment text is preserved by the lexer's trivia model: each meaningful token carries the line-comments and newlines that preceded it (back to the previous meaningful token), so the canonical-style emission can place comments at the right position. Horizontal whitespace and trailing whitespace inside comments are dropped on the way in; the comment's body is otherwise preserved byte-for-byte. The 100-column line-length budget does **not** apply to comments — long comments stay long.

**Top-level item comments.** Line comments immediately preceding a top-level `import` or item (`fn`, `type`, `literal`, `newtype`, `labels`, either `rec` group, `elab`, `op`, `equiv`, `bridge`) survive a `kio fmt` round-trip. They emit on their own lines at column 0, immediately above the item, in source order. The trivia stays attached to the item it preceded; for `import` statements that the formatter sorts into the canonical block layout, the comments ride with each `import` to its sorted position.

**File-header comments.** Line comments at the very top of a file, immediately preceding the file header — a `module …;` (regular or root module), a `package …;`, or a `dependency …;` — survive a `kio fmt` round-trip. They emit on their own lines at column 0, above the header, in source order, ahead of any `///` module doc-comment. (A `///` run directly above a module header is the module's doc-comment and rides the `///` rule below; the plain `//` header is what this clause preserves.)

**Doc-comments (`///`).** Doc-comments are emitted before the definition they document, at column 0, one `/// …` line per source line. The formatter normalizes each line's prefix to exactly `///` for a blank doc line (no content after the slashes) or `///` followed by a space and the line's content. The content itself is not reflowed — Kiodoc directives and Markdown indentation are preserved verbatim.

**Empty-comment-line collapse.** A run of two or more empty `//` lines (each one consisting of `//` after trailing-whitespace stripping, with no body) collapses to a single empty `//` line on emit. This composes with the blank-line policy: blank lines between items remain bounded by the surrounding item-separation rule (exactly one blank line). Doc-comment blank lines (`///` with no content) are not subject to this collapse — they are preserved verbatim.

**Comments are legal anywhere and are never dropped.** A `//` line comment may appear at any position the lexer accepts one; `kio fmt` preserves every comment across a round-trip. This is a hard guarantee, enforced by test (a conservation check that numbers every comment and asserts all survive in source order), not by a runtime guard. Two placement disciplines apply, the gofmt model:

- **Faithful at boundaries.** A comment at a node, container, or element boundary — above an item, above a `let` / statement / `match!` clause / list element, trailing the last token before a closing delimiter (`}` / `)`) or end of file, dangling at the end of a block — is preserved *in place*.
- **Hoisted otherwise.** A genuinely interstitial comment — wedged on a token that does not start an AST node (a `:`, `->`, `,`, or mid-expression position) — is hoisted to the nearest canonical line above the enclosing construct. It is relocated, never dropped, and source order is preserved across the relocation.

**Idempotence holds after the first pass.** Because an interstitial comment may be relocated on the first format, `kio fmt` is not guaranteed to be a strict no-op on every conceivable input; but the relocation reaches a fixpoint immediately, so `fmt(fmt(s)) == fmt(s)` for every `s`.

The precise indentation and line placement of a relocated comment is part of the canonical style's long tail; `kio`'s implementation is normative for the placement choices the spec under-specifies.

## Numeric and string literal canonicalization

The lexer accepts a generous input form for literals; the formatter normalizes that form to a single canonical shape on emit. The lexer already strips `_` digit separators in integer and float digit runs, so they don't survive into AST or output.

- **String `\uXXXX` escapes** — lowercase hex. The lexer accepts both `\u000B` and `\u000b` on input; the formatter emits `\u000b`. The named short escapes (`\n`, `\t`, `\r`, `\b`, `\f`, `\\`, `\"`) take precedence; only code points below 0x20 that don't have a named escape fall back to `\uXXXX`. A code point at or above 0x20 without a named escape emits as the raw character: `\u00FF`, `\u00ff`, and a raw `ÿ` on input all emit as `ÿ`.
- **Float exponents** — lowercase `e`, no leading `+`. `1.5E+10` and `1.5e10` both canonicalize to `1.5e10`; `1.5e-10` keeps the negative sign.
- **Integer and float digits** — emitted as the lexer-stripped form (no separators). `1_000_000` parses to digits `"1000000"` and re-emits as `1000000`.
- **Literal annotations** — a literal's optional `(Type)` annotation (the `LiteralCall` form) is emitted abutting the literal with no interior space: `42(I32)`, never `42 (I32)`. An unannotated literal stays unannotated — the formatter never adds an annotation a literal didn't carry.

## String literal reflow

A string literal whose **value** (the unescaped logical content, as stored on the AST) exceeds 72 characters is emitted as a sequence of adjacent string literals, one per line, with continuation chunks lined up at the column of the first `"`.

```kio
let greeting =
  "the quick brown fox jumps over the lazy dog and then continues"
  " running back home in the late afternoon sunshine"
```

Rules:

- **Break only at whitespace word boundaries.** A "word" is a maximal run of non-whitespace; chunks form by greedy fit (as many words as fit in 72 chars of value). Whitespace at the split point belongs to the preceding chunk (trailing space).
- **Unbreakable strings overflow.** A value with no whitespace (e.g., a URL or a long identifier-like string) cannot be split — it emits as one chunk that may exceed 72 chars.
- **Continuation chunks align with the first chunk's opening quote.** Determined by the column where the first `"` lands in the surrounding context.
- **The 100-column code budget does not apply to chunked literals.** Each chunk targets 72 characters of *value*, not 72 characters of *line position* — moving the literal to a deeper indent doesn't shrink the chunks.
- **Adjacent string-literal concatenation is a parser-level fold.** Two or more string-literal tokens with only trivia between them collapse into a single AST literal whose value is the concatenation. The chunking present in source is informational; the formatter re-chunks deterministically at 72 chars on emit, so two source-level chunkings of the same logical value both round-trip to the same canonical output.

## Comments inside expression bodies

Within an expression body, comments preserve through a `kio fmt` round-trip at the following positions. In each case the comment(s) emit on their own line(s) above the relevant construct, follow the same paragraph-separator and empty-comment-collapse rules that apply to top-level item comments, and force the surrounding `{ body }` block into multi-line layout (per § Block bodies):

- **Above a `let` binding** — comments captured before the `let` keyword.
- **Between `=` and the value** — comments captured between `=` and the start of the bound expression. The value emits on its own line at +2 indent. The `;` trails the value's last token wherever it lands. This same layout also fires whenever the flat `let X = E;` form would not fit on one line — because it overflows the 100-column budget, or because the value itself forces a break (e.g., trivia captured inside a call's argument list, or any other unconditional multi-line layout). Width pressure, between-`=`-and-value trivia capture, and an already-broken value share one canonical broken shape; the let breaks at `=` first and the value renders inside its +2 nest with whatever further breaks it requires.
- **Between `;` and the next statement / final expression** — comments captured between the statement-terminating `;` and the next token. (When the next statement is another `let`, that `let`'s own "above the `let` keyword" capture handles it; the formatter does not double-emit.)
- **Above a `match!` clause** — comments captured before the clause expression.
- **Inside a `match!` clause body** — comments captured between the clause's `{` and the body expression.
- **Inside a call's argument list** — comments captured before each argument's first token (or before the leading `,` separator). Any comment forces the call into the multi-line A1 layout, with the comment on its own line above its argument's `, arg` line.
- **Inside a tuple literal** — same per-element capture as call arguments. Any comment forces the tuple into the multi-line A1 layout. (Comments are kept through `kio fmt`'s round-trip; they are dropped on the way to `Desugared`, so they don't survive `kio-prime`'s re-emission.)
- **Inside a label-value `{f = e, g = e'}`** — same per-label capture as tuple elements; same lifetime caveat (dropped at the `label_elab` boundary).
- **Inside a type-application argument list** — comments above each type-arg in `Foo(A, B)` or `mk_foo(A, B, x)`. Forces multi-line A1 layout when any entry has a comment.
- **Inside a `labels { f: X, g: Y }` block** — comments above any label entry. (Same lifetime caveat as `Expr::LabelValue`: dropped on the way to `Lowered`.)
- **Trailing the last element of a comma list, before the `)` / `}`** — a comment after the final argument / tuple element / label / row-let binder emits on its own line below that element, inside the broken A1 layout, before the closer. The list breaks to multi-line because a comment can't sit inline before the closer.
- **Dangling at the end of a block, before `}`** — a comment after the body's last token emits on its own line at the body indent, before the closing `}`, forcing the block to multi-line layout.
- **Trailing the last item / at end of file** — a comment after the last top-level item, before end of file, emits at column 0 below the body, separated by one blank line like a top-level item.

```kio
fn f[A](v: A) -> . {
  // above the let
  let x =
    // between = and value
    compute();
  // between `;` and the next statement / final expression
  match! v {
    // above the clause
    .() {
      // inside the clause body
      x
    };
    .[B](n: B) -> . {
      g(
        // before the first arg
        , n
        // before the second arg
        , x
        )
    }
  }
}
```

`kio fmt` reformats a chain of `let X = E;` statements as a single line if the chain fits within the 100-column budget (the surrounding block stays inline) and as a vertical stack if any element forces a break — either because the chain overflows or because a comment is captured at any of the trivia positions above. Mixing the two within one chain isn't supported: a chain either all fits flat or all breaks. The same principle applies to `e;` expression statements interleaved with `let` statements.

Same-line trailing comments — a `// comment` on the same line as a preceding token — are preserved, but emit on the *next* line rather than trailing: the lexer attaches a `// comment` to the following token's leading trivia, and `kio fmt` always opens a fresh line for a comment. A trailing comment whose following token is a closing delimiter or end of file (where the leading-trivia model has no token to attach to) rides the enclosing container's trailing slot — the parser stashes the closer's / end-of-file's leading run there — so it survives at the boundary positions listed above instead of being dropped. The relocation onto a fresh line is the only sense in which `kio fmt` is not a strict no-op on first format; it is a fixpoint thereafter.

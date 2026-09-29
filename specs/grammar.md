# Grammar

This file is the canonical grammar contract for Kio. The prose specs reference into this file for productions; they keep the narrative explanations of *what* each form means and *how* it desugars. This file pins *what parses*.

It is organized in three layers; the Kio' grammar is syntactically nested inside the full Kio surface:

1. **[Kio' grammar](#kio-grammar)** — the strict subset of Kio that the `IS_KIO_PRIME` corpus marker asserts about a source file. This is what [`prime.md`](prime.md) commits to; codegen runs against this subset.
2. **[Kio surface extensions](#kio-surface-extensions)** — everything the full surface ([`language.md`](language.md)) adds on top of Kio'. Each surface form desugars to Kio' (or runs an elaborator pass that elaborates to Kio') before reaching codegen; see the prose pages for the per-form desugaring.
3. **[Package files](#package-files)** — the package-root file shapes, described in [`package.md`](package.md). A package file (`<name>.pkg.kio`) is an independent file shape: it starts with `package <name>;`, admits optional `build` and `bridge { … }` blocks in either order; the latter contains module-path globs that select which modules form the package's host boundary. There is no separate build-file shape. A dependency-declaration file (`<local>.dep.kio`) is a second package-root file shape: it starts with `dependency <local>;` and names one cross-package dependency. A dependency lock file (`<local>.lock.kio`) starts with `lock <local>;` and pins the commit a remote `git` dependency's `ref` resolved to.

A program that parses against the Kio' productions is *syntactically* Kio'. A program that parses against Kio' ∪ Kio surface extensions is *syntactically* Kio. Neither claim implies well-typedness or well-resolvedness — those are separate properties checked by later compiler phases.

## Notation

`'token'` is a literal terminal. *PascalCase* names are non-terminals. `?` marks an optional element; `*` zero-or-more; `+` one-or-more. `|` separates alternatives. Whitespace, `// …` line comments, and `/// …` doc-comment lines are lexer trivia and may appear between any two tokens; they are not standalone productions — doc-comment attachment is described in the per-production notes below.

**Dictionary-like entries are unordered.** Where a definition body or package-root
configuration section consists of named fields or sections, those entries may
appear in any input order. Their productions repeat an entry alternative;
accompanying constraints specify required entries, duplicates and incompatible
combinations at each owner's specified validation boundary. Canonical print order
belongs to [`style.md`](style.md), not input grammar. Order within entry operands,
ordinary declaration and statement sequences, and signature history replay keeps
its separately specified meaning.

**Comma and operator runs.** Every comma-separated production in this file — uses of `(',' X)*` — is shorthand for `(','+ X)* ','*` with an optional leading `','*` immediately after the opener: items are separated by *one or more* commas, with optional comma runs at either edge. Tuple forms collapse zero items to `()`, one item to grouping, and two or more items to Kio's right-associated binary tuple shape. Product types do not use commas; they use `&`. The type-chain productions (both the unparenthesized `TypeChainTail` and the parenthesized `'(' Type '&' Type ')'` / `'(' Type '|' Type ')'` forms) admit any number of operators in any position (leading, interior, trailing) — `A & B & C`, `(A & B & C)`, `& A & B`, and `( &&& A &&& B &&& )` all parse to the same product. Empty product chains parse as `.`, empty sum chains parse as `!`, and one-item chains parse as grouping. Adjacent `STR_LIT` tokens fold into a single literal. These rules collapse to the canonical one-separator-per-item layout at the AST level — they exist so `kio fmt` can emit the leading-comma + leading-operator multi-line layouts uniformly. See [`language.md` § Comma-separated lists and operator chains](language.md#comma-separated-lists-and-operator-chains) and [`language.md` § Literals](language.md#literals).

**Semicolon boundaries.** A complete outer braced declaration or section
accepts at most one redundant suffix `;`. Nonbraced outer entries retain their
terminator. Braces inside an entry do not change that classification:
`host type T { owned };` has a qualifier, and `labels N = { x: . };` has a
braced operand, not a declaration body. Inside a semicolon-delimited block,
the enclosing sequence owns the separators between entries. Edge separators
are allowed under the sequence's repeated-separator policy; comma lists and
expression blocks retain their separately specified rules.

The parameterized productions below abbreviate the two separator policies:

```ebnf
SemiEntries<Entry>    ::= ';'? Entry (';' Entry)* ';'?
SemiRunEntries<Entry> ::= ';'* (Entry (';'+ Entry)* ';'*)?
```

`SemiEntries<Entry>` is nonempty and admits only single separators. An
optional use of that whole production admits an empty body, not a
semicolon-only body. `SemiRunEntries<Entry>` admits runs, including in an
otherwise empty body. The enclosing construct's required entries, uniqueness,
and minimum cardinality still apply. Canonical formatting emits separators
between entries, with none at either edge; see [style.md](style.md#semicolon-clause-blocks).

## Kio' grammar

The Kio' grammar covers the **regular-module** file shape — every `*.kio` file with a `module` declaration. Package boundary files have their own grammars in [§ Package files](#package-files).

### Lexical structure (Kio')

- `IDENT` — `[A-Za-z_][A-Za-z0-9_]*`. Naming conventions distinguish *type names and type parameters* (`_?[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*`) and *value/parameter names* (`_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*`). Each word contains letters followed by optional digits; exactly one underscore separates internal words. Leading and trailing underscores are affixes, not empty words. User identifiers admit at most one leading underscore; two or more reserve the spelling for the compiler, without requiring a trailing underscore. Reserved names obey the same word and role rules. Parser sites validate the role-specific convention they require. `_Foo` and `_Foo123_bar4` are type-shaped, `_foo` is value-shaped, and `_1Foo`, `FooBar`, `foo_123`, `foo__bar`, and `a1b` match neither user role. Every name contains at least one ASCII letter. Bare `_` is the wildcard binder (`let _`, a discarded parameter or neutral-block binder) and, doubled/tripled, an operator slot token (`_` / `__` / `___`); it is admitted only at those binder/slot positions, never as a name. In type expressions, the final segment of a type path must be a type name or reserved compiler-internal type name; lowercase identifiers remain value syntax. Keyword-shaped words, including `true` and `false`, lex as `IDENT` unless another lexical rule for their surrounding punctuation applies.
- `INT_LIT` — a digit sequence, optionally grouped with `_` digit separators. No width suffix: in Kio' the literal's type comes from its mandatory `LiteralCall` annotation (see [`language.md` § Literals](language.md#literals)). A digit sequence directly followed by an identifier character (`42i32`) is *not* one token — it lexes as `INT_LIT` then `IDENT`, which the parser cannot place.
- `FLOAT_LIT` — a digit sequence with a `.`-fractional part and/or an exponent (`e`/`E`, optional sign), optionally grouped with `_` digit separators. No width suffix. A digit sequence with neither a fractional part nor an exponent is `INT_LIT`, not `FLOAT_LIT`.
- **Negative-literal carve-out.** A leading `-` flush against the first digit of an `INT_LIT` / `FLOAT_LIT` (no whitespace between) is consumed as part of the literal token when, and only when, the **prior token is not expression-ending**. Identifiers, literals, `)`, and `}` are expression-ending; start-of-file and every `SymbolRun` — including a standalone `]` operator run — are expression-starting. After an expression-ending token the `-` is its own symbol token (so `a-1`, `1-2`, `(x)-1`, and `}-1` are binary-subtraction shapes), and `- 40` (whitespace between `-` and the digit) is always two tokens. This is a **shared lexer rule** — it applies identically in Kio' and in Kio surface. The surface-extension framing with worked examples is in [§ Notes (Kio surface)](#notes-kio-surface).
- `STR_LIT` — `"…"` with JSON-style escapes: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, and `\uXXXX` for code points in the Basic Multilingual Plane.
- Structural punctuation tokens, each its own token kind: `(` `)` `{` `}` `,` `;`.
- Symbol runs use one shared lexical rule in Kio and Kio': an `OpToken` is a **maximal contiguous run** drawn from ``+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | : . [ ]``. The Kio' productions admit only the fixed grammatical roles they name (`<` `>` `[` `]` `:` `=` `|` `&` `!` `.` `/` `*` `->`); user-operator expressions and declarations still have no Kio' production. `*` marks a `TypeBinder`'s kind annotation, and an all-star run carries its arrow count. `&` / `|` runs collapse as chain separators. `/` is admitted as a module-path separator only. At a grammar position that requires a structural prefix, the parser may peel that prefix from the maximal run: function `->`, an existential closer `>`, a forall opener or closer `[` / `]`, a unit `.`, and the documented chain separators. Thus compact forall spellings such as `[*F]`, `[A][B]`, and `.[*F]` remain valid even though their raw runs include `[*`, `][`, or `.[*`. This recovery is parser-contextual; no operator-pattern or expression path decomposes a bracket-bearing run. At every other fixed-symbol position a fused spelling is a parse error: `type A=.;` does not parse — its flush `=.` is one run, not the required `=` — while `type A = .;` does.

**No user-operator productions in Kio'.** Kio' uses the shared maximal-run lexer, then admits a run only where the grammar assigns its spelling a fixed role. Sequential parser-contextual peeling may expose adjacent fixed delimiters such as the `][` between forall groups, but it never turns a run into an operator expression or pattern. The surface language's `op` or `varop` item, tagged operator import, and operator-call productions are absent from Kio'.

**Contextual keywords (Kio').** The words `module`, `import`, `as`, `pub`, `pure`, `type`, `fn`, `rec`, `newtype`, `let`, `constructor`, `projector`, `host`, and the magic phrase `__intrinsics__` lex as ordinary `IDENT` tokens. They are recognized as keywords *only* in the grammatical positions where the productions below show them as literal terminals; in any other position the same spelling is an identifier. `pure` is a modifier only on an ordinary `fn` declaration. `rec` marks either one self-recursive `newtype` or one mutually recursive type group; it takes no runtime capability in Kio'. `host` is a host-declaration modifier only when immediately followed by `type` or `fn` (optionally with a redundant `pub` between them); elsewhere `host` is an ordinary identifier, so existing code that uses `host` as a name keeps parsing. Package-boundary words such as `package` and `bridge` are recognized only inside the package shapes that admit them. (`file` is not a Kio grammar keyword; it is a kiodoc fence attribute — see [`kiodoc.md`](kiodoc.md).)

**Comment markers require trailing whitespace.** A comment marker — `//` (line comment) or `///` (doc-comment line) — must be followed immediately by whitespace, end-of-line, or end-of-file. A non-whitespace character flush against the marker is a lex error: `//foo`, `///bar`, and `//=` are all rejected, with no exception for alphanumeric or underscore characters. In particular a fourth slash (`////`, `////////`) is rejected — four or more slashes are **not** a section ruler. This reserves the whole `//…` operator family (`//!`, `//?`, `//=`, …) for future syntax (see [§ Lexical structure (Kio surface)](#lexical-structure-kio-surface) for the matching `op`-pattern reservation).

**Doc-comment lines (`///`).** A line whose first non-whitespace characters are `///` followed by whitespace, end-of-line, or end-of-file is a doc-comment line. It is lexer trivia (like `//`) but carries its payload to the parser. Doc-comment lines attach to the immediately following top-level declaration (see `DocAttached` in [§ Productions (Kio')](#productions-kio)); a `///` block before an `import` clause is a parse error.

### Productions (Kio')

Imports precede module items. Selective lists are nonempty, including when the
comma-list rule admits commas at their edges. In canonical formatting, the
opening `(` immediately follows the module path on the introducing line.
Qualified imports require
`as` and a value-shaped alias. `__intrinsics__` admits only its builtin block
form; it is not an ordinary selectively or qualified-importable module.

```ebnf
ModuleFile        ::= DocBlock? ModuleDecl Import* Item*
ModuleDecl        ::= 'module' ModulePath ';'
ModulePath        ::= IDENT ('/' IDENT)*      (* module reference, `/`-separated segments *)
ValuePath         ::= IDENT ('.' IDENT)*      (* lexical value path; `/` is not admitted *)

DocBlock          ::= DOC_LINE+               (* contiguous run of `///` lines *)
DOC_LINE          ::= '///' &(WS | EOL | EOF) <to end of line>
                                              (* `///` must be followed by whitespace / EOL / EOF;
                                                 `////`+ is a lex error, not a comment ruler *)

Import            ::= ImportEntry ';'
ImportEntry       ::= ImportIntrinsics | ImportSelective | ImportQualified
ImportIntrinsics  ::= 'import' '__intrinsics__'
ImportSelective   ::= 'import' ModulePath '(' IDENT (',' IDENT)* ')'
ImportQualified   ::= 'import' ModulePath 'as' IDENT (* alias is a value name *)

Item              ::= DocBlock? ((FnDef | Newtype | RecursiveNewtype | TypeRecGroup) ';'? | TypeAlias | HostType | HostFn)
                                              (* DocBlock attaches to immediately following item;
                                                 DocBlock before Import is a parse error *)
Vis               ::= 'pub' VisScope?         (* `pub` exports; `pub(a/b)` seals export to the module subtree rooted at `a/b` *)
VisScope          ::= '(' ModulePath ')'      (* `a/b` must be a prefix of the declaring module's path — see language.md § Visibility. Enforced at resolution; the item is then kept out of the host-export surface. *)
FnDeclModifier    ::= Vis | 'pure'
FnDeclModifiers   ::= FnDeclModifier*         (* at most one visibility and at most one `pure`; fmt emits `pub pure` *)
FnDef             ::= FnDeclModifiers 'fn' IDENT Signature ('->' Type)? Block
TypeAlias         ::= TypeAliasBody ';'
TypeAliasBody     ::= Vis? 'type' IDENT TypeParamList? '=' Type
Newtype           ::= Vis? NewtypeAfterVis
RecursiveNewtype  ::= Vis? 'rec' NewtypeAfterVis
NewtypeAfterVis   ::= 'newtype' IDENT TypeParamList? ExistsBinder* ':' Type NewtypeBody
NewtypeBody       ::= '{' SemiRunEntries<NewtypeMember> '}'  (* exactly one constructor and one projector *)
NewtypeMember     ::= Vis? 'constructor' IDENT
                    | Vis? 'projector' IDENT

TypeRecGroup      ::= 'rec' '{' SemiEntries<TypeRecMember> '}'  (* at least two members *)
TypeRecMember     ::= DocBlock? (TypeAliasBody | Newtype)

HostType          ::= HostTypeBody ';'
HostTypeBody      ::= 'pub'? 'host' 'pub'? 'type' IDENT TypeParamList? RoleAnnotation? OwnedBlock?
HostFn            ::= HostFnBody ';'
HostFnBody        ::= 'pub'? 'host' 'pub'? 'fn' IDENT Signature '->' Type
RoleAnnotation    ::= 'role' '(' IDENT ')'    (* IDENT ∈ i8…i128 / u8…u128 / f32 / f64 / bool / str *)
OwnedBlock        ::= '{' 'owned' '}'

TypeParamList     ::= TypeBinderGroup+
Signature         ::= SignatureGroup+                     (* must include at least one ValueParamGroup *)
FnSignature       ::= FnSignatureGroup+                   (* must include at least one FnValueParamGroup *)
SignatureGroup    ::= TypeBinderGroup | ValueParamGroup
FnSignatureGroup  ::= TypeBinderGroup | FnValueParamGroup
ValueParamGroup   ::= '(' SignatureParamList? ')'
FnValueParamGroup ::= '(' FnSignatureParamList? ')'
SignatureParamList ::= SignatureParam (',' SignatureParam)*
SignatureParam    ::= IDENT ':' Type                      (* value parameter *)
FnSignatureParamList ::= FnSignatureParam (',' FnSignatureParam)*
FnSignatureParam  ::= IDENT (':' Type)?                   (* value parameter; annotation optional *)
TypeBinderGroup   ::= '[' TypeBinder (',' TypeBinder)* ']'
TypeBinder        ::= '*'* IDENT                          (* kind-annotated binder; n leading stars = n arrows *)

Type              ::= TypeArrow
TypeArrow         ::= TypeBinderGroup+ TypeArrow          (* forall, prefix-only *)
                    | TypeArrowParam '->' TypeArrow       (* function type; right-associative *)
                    | TypeChain
TypeArrowParam    ::= TypeAtom                            (* product/sum LHS requires parentheses *)
TypeChain         ::= TypeAtom TypeChainTail?
TypeChainTail     ::= ('&' TypeAtom)+                     (* unparenthesized product chain *)
                    | ('|' TypeAtom)+                     (* unparenthesized sum chain     *)
TypeAtom          ::= '!'                                 (* bottom             *)
                    | '.'                                 (* unit               *)
                    | '(' Type ')'                        (* grouping           *)
                    | '(' Type ('|' Type)+ ')'            (* parens sum chain   *)
                    | '(' Type ('&' Type)+ ')'            (* parens product     *)
                    | TypePath
TypePath          ::= TypeName TypeArgList?
                    | ModulePath '.' TypeName TypeArgList?
TypeName          ::= IDENT                               (* type-word shape, with user or reserved underscore prefix *)
ExistsBinder      ::= '<' IDENT '>'
TypeArgList       ::= '(' Type (',' Type)* ')'

Block             ::= '{' BlockBody '}'
BlockBody         ::= ';'* (Stmt ';'*)* Expr ';'*
Stmt              ::= LetStmt | ExprStmt
LetStmt           ::= 'let' LetBinder '=' Expr ';'
LetBinder         ::= IDENT | '_'
ExprStmt          ::= Expr ';'
Expr              ::= ExprPostfix
ExprPostfix       ::= ExprAtom ExprSuffix*
ExprSuffix        ::= '(' (CallArg (',' CallArg)*)? ')'
CallArg           ::= Expr | Type                       (* see notes below  *)
ExprAtom          ::= LiteralCall
                    | '(' ')'                             (* unit value       *)
                    | '(' Expr ')'                        (* grouping         *)
                    | ValuePath
                    | FnExpr
LiteralCall       ::= (INT_LIT | FLOAT_LIT | STR_LIT+ | BoolLit) '(' Type ')'
BoolLit           ::= '.' ('t' | 'f')                     (* token sequence: `.` plus identifier `t` / `f` *)
FnExpr            ::= '.' FnSignature ('->' Type)? Block
```

### Notes (Kio')

- `(` introduces several productions in both type and expression positions; in type position it is grouping unless the contents form a parenthesized `&` / `|` chain. A grouped type may be followed by `->`, so `(A) -> R` is the same function type as `A -> R`, and `(A & B) -> R` uses the grouped product as the function domain. Type position has no comma-product production and empty parentheses are not a type: `(A, B)` and `()` are not type expressions. The grammar above is unambiguous given one-token lookahead after the opening `(` — extended to also recognize a leading `&` / `|` as a chain opener (the formatter's multi-line layout).
- Type-binder groups inside type expressions are prefix-only. `[A] A -> R` and `[A] (A & B) -> R` are valid forall-prefixed types; `[A] -> R` is rejected because the binder run has no body type.
- Mixing `&` and `|` in one parenthesized chain remains a parse error — `(A & B | C)` is ill-formed; users wanting both must explicitly parenthesize as `(A & (B | C))` or `((A & B) | C)`. Bare unary arrows are right-associative (`A -> B -> C` is `A -> (B -> C)`), but their left side is atomic: a product/sum parameter must be parenthesized (`(A & B) -> C`, not `A & B -> C`; `(A | B) -> C`, not `A | B -> C`). The arrow's right side is a full `Type`, so `A -> B & C` parses as `A -> (B & C)`. Mixed chains still reject: `A & B | C` and `A | B & C` are ill-formed without explicit grouping. Leading, trailing, and repeated arrow-token runs are not accepted.
- `ValuePath` is the lexical path shape for value expressions. A bare head is
  local or selectively imported; dotted segments select through an explicitly
  imported module alias or name a local/imported newtype member, directly or
  through a fully saturated positional identity alias. The dots are part of
  the path, not general member access on an arbitrary expression. A
  slash-separated `ModulePath` is not a value expression; importing the item
  or module establishes the cross-module binding first.
- Call-argument lists are uniform in `ExprSuffix`: type arguments and value arguments share one comma-separated list. The typer pairs them positionally against the callee's signature. For identifier-shaped spellings admissible as both a value and a type (a plain identifier or a parametric call like `Foo(A, B)`), the spelling resolves the ambiguity before declaration lookup: an exact type-name spelling (`A`, `_A`, `Foo123_bar4`) is type-shaped, while an exact value-name spelling (`x`, `_x`, `foo123_bar4`) is value-shaped. A spelling matching neither role is rejected rather than falling back to the other namespace. The unit spelling `()` parses only as a value; its type is `.`. For slots that can only be a type (the bottom `!`, paren-wrapped type forms `(A & B)` / `(A | B)`, function types `T -> R` / `(T & U) -> R`), only the `Type` reading parses.
- `LiteralCall` is a literal token or parser-recognized literal token sequence with a mandatory trailing `'(' Type ')'` annotation — `42(I32)`, `3.14(F64)`, `"hi"(String)`, `.t(Bool)`. The annotation pins the literal's role-bearing host type. See [`language.md` § Literals](language.md#literals); Kio' admits no unannotated-literal form. The annotation is parsed by the same `'(' … ')'` machinery as a call-argument list, so a parser may treat it as an `ExprSuffix` on the literal rather than as a distinct production; either way the result is one literal node carrying its annotation. The Kio-surface extensions section below re-introduces the unannotated form additively for the full surface language, where the three-tier resolution rule in [`language.md` § Literals](language.md#literals) decides the literal's type from context.
- **Higher-kinded type application** is the ordinary `TypePath` shape applied to a kind-`*→*`-or-higher binder or newtype head: `F(A)` where `[*F]` is a kind-`*→*` binder, `Either(String)` where `Either` is an arity-2 newtype. Both parse to one `Type::Path { segments, args }`; there is no dedicated application node and no `__App__` keyword. The kind discipline (§ Kind grammar) decides admissibility — a kind-`*` head applied to an argument is a kind error, surfaced by semantic kind checking rather than the parser. A parametric transparent alias uses the same parsed shape but is well-formed only when every parameter declared by that alias has an argument; underapplication cannot retain the alias's own binders. See [`prime.md` § Kio' as Church-style fragment of System F-ω](prime.md#kio-as-church-style-fragment-of-system-f-ω) and [`language.md` § Higher-kinded types](language.md#higher-kinded-types).
- `pub` is a contextual keyword only in two positions: leading an `Item` and leading a `NewtypeMember`. Elsewhere the same spelling is an `IDENT`.
- A `NewtypeBody` holds an **unordered set** of members: exactly one `constructor` and exactly one `projector`, in **either order** (`constructor` then `projector`, or `projector` then `constructor`). Both must be present and neither may be duplicated; the order carries no meaning. `kio fmt` canonicalizes the pair to `constructor`-then-`projector` (see [`style.md`](style.md)).
- `RecursiveNewtype` is the singleton recursive-data spelling. Its `rec` follows visibility (`pub rec newtype`); a self-recursive newtype requires it, while a marker on an acyclic newtype is rejected as redundant. An ordinary `Newtype` has no own-head or later-declaration scope.
- `TypeRecGroup` is capability-free and has no group-level visibility. It contains at least two ordinary `type` / `newtype` members, with visibility and doc comments attached to each member. The complete member heads are in scope throughout the group. Static validation requires the written group to be exactly one genuinely mutual cyclic component and requires every alias cycle to cross a `newtype` boundary; a one-member, acyclic, multi-component, or alias-only group is rejected. A member does not repeat the singleton `rec` marker.
- The keyword `'fn'` leads only item declarations (`fn IDENT (…)`). Anonymous functions use `.(…) { … }`, so expression position does not reserve `fn`.
- A module's `import` clauses all precede its items in one flat run (`ModuleDecl Import* Item*`); there is no `import`-block split. `import m(X);` creates a `→ m` import edge wherever it sits. See [`language.md` § Module cycles](language.md#module-cycles).
- `HostType` and `HostFn` are opaque, signature-only **host declarations** — a `host type` is a nominal type the host supplies, a `host fn` a function whose body the host supplies. They are ordinary module items (not a special block), are always public, and obey ordinary lexical scoping: another module reaches a `host` item through a plain `import`, exactly like any other `pub` item. `host` is a contextual keyword (see § Contextual keywords above). A `host type`'s optional `role(...)` annotation pins the type to a host-supplied atomic value and drives literal reception (see [`language.md` § Literals](language.md#literals)). `TypeParamList` and `RoleAnnotation` are individually syntactic components, but host type parameters must be unstarred kind-`*` binders and are semantically mutually exclusive with `RoleAnnotation`: a role-bearing `host type` has no type parameters. Violating either restriction is a type error. The optional `{ owned }` block remains accepted and formatted for source compatibility, but selects no alternate representation: the Rust backend renders every `role(str)` facade in owned form at every occurrence (see [`backends/rust.md`](backends/rust.md)). A `host fn`'s value parameters carry the same named, annotated `SignatureParam` shape as an ordinary `FnDef`, and its `-> Type` return is mandatory (a `host fn` has no body to infer it from). `pub host` / `host pub` are admitted but redundant — there is no private host item. Host items are part of Kio': they appear in the boundary AST and reduce as opaque primitives. Which modules' host items reach the host is selected by the package file's `bridge` block (see [§ Package files](#package-files)).

The `IS_KIO_PRIME` corpus marker (see [`test-data/README.md`](../test-data/README.md)) asserts that every regular-module file in a case parses against the Kio' productions above. The corpus check `ci/checks/orchestrators/golden-tests.sh` verifies acceptance via the standalone `kio-prime-check` tool under [`ci/infra/kio-prime-check-rs/`](../ci/infra/kio-prime-check-rs/).

### Kind grammar

Every type binder and every type constructor has a **kind** — the type-level arity that decides how many type-arguments it accepts. The kind language is a right-associative chain over a single base kind:

```ebnf
Kind              ::= '*'                                 (* the kind of ordinary (saturated) types *)
                    | '*' '→' Kind                        (* a type-level function, right-associative *)
```

So `*` is the kind of a fully-applied type (`String`, `(A | B)`, `Box(A)`), `*→*` is the kind of an arity-1 constructor (`Box`, `List`, a `[*F]` binder), `*→*→*` is the kind of an arity-2 constructor (`Either`, `Result`, a `[**G]` binder), and so on. The chain is right-associative: `*→*→*` parses as `*→(*→*)`. There are **no kind variables** and **no higher-order kinds** — the domain of every arrow is `*`, never another arrow. This keeps the kind language a closed, finite, structurally-decidable lattice (see [`prime.md` § Kio' as Church-style fragment of System F-ω](prime.md#kio-as-church-style-fragment-of-system-f-ω)).

The **binder-annotation surface** encodes a kind as a run of leading `*` characters before the binder name inside a `TypeBinderGroup`. The star count `n` is the number of arrows:

| Surface | Kind | Meaning |
|---|---|---|
| `[A]` | `*` | ordinary type-parameter (the default; no stars) |
| `[*F]` | `*→*` | arity-1 type constructor |
| `[**G]` | `*→*→*` | arity-2 type constructor |
| `[***H]` | `*→*→*→*` | arity-3 type constructor |

A newtype's kind is read from its declared parameter list, not annotated separately. Each parameter carries its own kind (the default `*`, or a starred annotation), and the newtype accepts one argument per parameter, checked against that parameter's kind. An ordinary arity-`n` newtype whose parameters are all kind-`*` (`List[A]`, `Either[E][A]`) accepts `n` kind-`*` arguments and saturates to kind `*`. A *dictionary* newtype takes a higher-kinded parameter — `Monad[*F]` accepts one kind-`*→*` argument (a one-argument type constructor) and saturates to `*` — so `Monad(Maybe)` and `Monad(Either(String))` check the argument's kind (`*→*`) against the `[*F]` parameter. (This per-parameter check is the newtype's signature; it is *not* a higher-order kind in the `κ` language above — a binder still cannot be annotated `[(*→*)→* …]`.) **Kind annotations are part of a type's public signature** — they carry unchanged across package boundaries; see [`package.md`](package.md). Kinds are never inferred: the user writes the stars at every higher-kinded binder, and a binder with no stars is kind `*`.

**Type application consumes one parameter per argument, matching kinds.** A binder or newtype head whose parameters are `*→*→*`-shaped (`Either[E][A]`) applied to one argument (`Either(String)`) leaves the remaining parameter, so the partial application has kind `*→*`; applied to two (`Either(String, I32)`) it saturates to `*`. A parametric transparent alias instead requires exactly one argument per declared parameter: both its bare name and every undersaturated application are type errors. Applying a kind-`*` head to any argument, supplying more arguments than parameters, or supplying an argument whose kind doesn't match the parameter's kind, is a kind error.

## Kio surface extensions

The full Kio surface language extends Kio' with additional tokens, additional contextual keywords, and additional productions on existing non-terminals (plus a few new top-level item shapes). Everything in [§ Kio' grammar](#kio-grammar) carries through unchanged; this section spells out what's *additive*. The `+=` notation below means "add these alternatives to the named production from the Kio' grammar"; `::=` introduces a new production.

Each surface form's prose meaning (what it means, how it desugars) lives in [`language.md`](language.md); the productions here pin *what parses*. For the elaborator bang-call mechanism — how `IDENT '!'` resolves as a user-defined elaborator call — see [`language.md` § Elaborators are imported, not ambient](language.md#elaborators-are-imported-not-ambient); the reference coercion palettes' per-form semantics are documented as case studies in [`docs/poc/optics.md`](../docs/poc/optics.md) and [`docs/poc/elab.md`](../docs/poc/elab.md).

### Lexical structure (Kio surface)

Kio surface admits everything Kio' admits plus:

- `OpToken` — the shared maximal `SymbolRun` class over ``+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | : . [ ]``. There is no standalone-vs-fused distinction: `[!`, `]]`, `]-`, and `][*` are each one run, while whitespace makes `[ !`, `] ]`, `] -`, and `] [ *` distinct run sequences. Dotted runs are admissible as user operator tokens when they do not start with `.` (`+.` / `<.>`) or, if they do start with `.`, when they contain at least two dots (`..` / `.+.`); leading-dot runs with exactly one dot (`.`, `.>`, `.+`, `.###$`) are reserved. Whitespace, identifiers, digits, and structural punctuators (`, ; ( ) { }`) break a run. See [`language.md` § Operators](language.md#operators) for per-pattern admissibility.
- Placeholder stems and numbered references are ordinary `IDENT` tokens. The adjacent `.stem.` introduction establishes source-local ownership; no operator-character marker or indexed-token fusion participates.

The symbols `< > [ ] = ! : . / -> .? .! .> .>> .< .<<` appear in fixed structural roles in many productions: `<` and `>` bracket existential binders (`<U>`); `[` and `]` bracket forall binders; `!` is the bottom type and bang-call suffix; `=` introduces binding bodies; `:` introduces type annotations; `.` appears in lexical value paths, type-path qualification, member access, and row lets; `/` separates module paths; `->` is the function arrow; and the dot-prefixed forms have their documented access/update/splice roles. The lexer makes no token-kind distinction between a structural spelling and the same character inside a longer run, so productions disambiguate by parser position. At a required structural position the parser peels the delimiter from the front of a fused run and leaves the residual for the next production. This includes the two forall brackets, preserving `[*F]`, `[A][*F]`, and `.[*F]`. User-operator declarations, expressions, variadic operators, and imports never use this recovery: they read each maximal run whole.

### Contextual keywords (Kio surface)

In addition to Kio''s contextual keywords, Kio surface recognizes:

- `trailing`, `product`, `thunk`, `sequence` — trailing-block descriptor entries in an elaborator declaration.
- `rec` — the Kio' recursive-data marker plus the surface recursive-function group lead and recursive call marker. `rec newtype` and bare `rec { ... }` retain their Kio' meanings; `rec labels` and `rec(loop)` are surface additions.
- `labels`, `type`, `literal`, `elab`, `op`, `varop`, `equiv` — top-level item leads (after declaration modifiers where admitted). A singleton recursive declaration admits leading visibility (`pub rec labels`, `pub rec newtype`, or `pub rec(loop) fn`); either braced `rec` group carries no visibility, so visibility stays on each member.
- `__comptime__` — reserved import target, recognized only inside `import __comptime__;` (see [§ Surface import-clause additions](#surface-import-clause-additions)). Never a regular module name. Surface-only — it supplies the compile-time reflection API used by elaborator implementations.

Plus the bang-call forms — token sequences (`IDENT` + `Bang`) rather than single keywords; the parser recognizes the conjunction when an identifier is immediately followed by `Bang`. Bang calls resolve as user-defined elaborator calls through ordinary scoped imports — there are no reserved elaborator keywords; any `IDENT '!'` whose name is in scope as an imported `elab` declaration is a bang-call. Each call is checked against the imported elaborator declaration's own call type. The reference distributions ship source-to-target coercion palettes plus `match!` (dispatch), `derive!` (instance deriving), and others, but these are user-defined Kio libraries, not grammar keywords; their per-form semantics are documented as case studies in [`docs/poc/optics.md`](../docs/poc/optics.md) and [`docs/poc/elab.md`](../docs/poc/elab.md), and the call mechanism in [`language.md` § Elaborators are imported, not ambient](language.md#elaborators-are-imported-not-ambient).

### Surface item additions

```ebnf
Item += Labels | RecursiveLabels | LabelForward | LiteralAlias
      | (ElaboratorItem | Op | VariadicOperator | Equiv | RecItem) ';'?

Labels         ::= Vis? LabelsAfterVis
RecursiveLabels ::= Vis? 'rec' LabelsAfterVis
LabelsAfterVis ::= 'labels' LabelsForm
LabelsForm     ::= LabelsBody ';'
LabelsBody     ::= LabelsProduct                                    (* anonymous product *)
                    | IDENT TypeParamList? '=' LabelsSum             (* named product/sum alias *)
LabelsSum      ::= LabelsProduct ('|' LabelsProduct)*
LabelsProduct  ::= '{' LabelEntry (',' LabelEntry)* ','? '}'
LabelEntry     ::= IDENT TypeParamList? ExistsBinder* ':' LabelPayload
LabelPayload   ::= Type                                            (* exact bare `_` is a reuse marker here *)
LabelForward   ::= Vis? 'type' '{' IDENT '}' '=' '{' LabelPath '}' ';'

LiteralAlias   ::= Vis? 'literal' IDENT '=' LiteralAliasValue ';'           (* lowercase name only *)
LiteralAliasValue ::= INT_LIT | FLOAT_LIT | STR_LIT | BoolLit

ElaboratorItem ::= Vis? 'elab' IDENT ':' Type ElaboratorBody
ElaboratorBody ::= '{' SemiEntries<ElaboratorEntry> '}'
ElaboratorEntry ::= ElaboratorCaptures | ElaboratorImpl | TrailingBlockDecl
ElaboratorImpl ::= 'impl' ValuePath
                 | 'impl' '(' 'fills' ')' ValuePath
ElaboratorCaptures ::= 'captures' (CapturePath | '(' CapturePath (',' CapturePath)* ','? ')')
TrailingBlockDecl ::= 'trailing' BlockExposure IDENT?
BlockExposure  ::= 'product' | 'thunk' | 'sequence'
CapturePath    ::= IDENT ('.' IDENT)*

RecItem        ::= Vis? 'rec' '(' ValuePath ')' RecFnDef                 (* exact one-member braced expansion; at most one visibility total — before `rec` (fmt-canonical) or on the fn *)
                 | 'rec' '(' ValuePath ')' '{' SemiEntries<RecFnDef> '}' (* grouping braces carry no visibility; each member does *)
RecFnDef       ::= Vis? 'fn' IDENT Signature ('->' Type)? Block

TypeRecMember += DocBlock? Vis? 'labels' LabelsBody

Op             ::= Vis? 'op' OpBody
OpBody         ::= OpPattern '{' SemiEntries<OpEntry> '}'                 (* fixed arity only *)
OpEntry        ::= 'impl' ValuePath                                      (* exactly once *)
OpPattern      ::= OpPatternStep+                                       (* must include >= 1 FixedOpToken *)
OpPatternStep  ::= OpPart
                    | '(' OpPart+ ')'                                      (* lenient-grouping marker; doesn't nest *)
OpPart         ::= '_' | '__' | '___'                                      (* slots: plain / recursive / greedy *)
                    | FixedOpToken
                    | '(' FixedOpToken ')'                                    (* operator-token quotation *)
FixedOpToken   ::= OpToken                                                   (* contains neither `[` nor `]` *)
VaropHead         ::= VaropOpen VaropClose                                  (* whitespace is required between the two runs *)
VaropOpen         ::= OpToken                                               (* one nonbare run containing `[` and no `]` *)
VaropClose        ::= OpToken                                               (* reverse OPEN and replace each `[` with `]` *)
VariadicOperator ::= Vis? 'varop' VaropHead VariadicBody
VariadicBody      ::= '{' SemiEntries<VariadicEntry> '}'
VariadicEntry     ::= VariadicPrimary | 'finalize' ValuePath
VariadicPrimary   ::= 'foldl' ValuePath ValuePath      (* step, nullary base *)
                   | 'foldr' ValuePath ValuePath      (* step, nullary base *)
                   | 'foldl1' ValuePath ValuePath     (* step, unary first-element seed *)
                   | 'foldr1' ValuePath ValuePath     (* step, unary last-element seed *)

Equiv             ::= 'equiv' IDENT Signature?
                    '{' ';'* Expr (';'+ Expr)+ ';'* '}'                       (* N >= 2 arms *)
```

The fixed operator body has exactly one `impl` entry. The elaborator body has
exactly one implementation entry, at most one captures entry, and zero or more
trailing-block entries. Entries may be interleaved in any order; the relative
order of trailing-block entries declares block order. The first descriptor is
unlabelled; every subsequent descriptor has a distinct value-shaped label.
The variadic body has exactly one primary clause and at most one finalizer,
in either order; different primary modes are mutually exclusive.
`foldl` and `foldr` accept an empty literal and invoke a
nullary base; `foldl1` and `foldr1` require an element and seed from the first
or last element respectively. Each element is one ordinary expression. The
step receives the accumulator first for left modes and last for right modes;
an optional unary finalizer runs once and may change the result type. The full
expansions are specified in [language.md](language.md#operators).

`VaropOpen` and `VaropClose` are whole maximal runs, not sequences of runs.
Both satisfy the shared symbol-run and reserved-spelling rules; the closing
run must be the exact mirror, such as `[*` / `*]`, `*[` / `]*`, or `[[` / `]]`.
The head contains whitespace between the two runs and has no slots or separator
declaration. Fixed `OpPart` tokens, including quoted tokens, contain neither
`[` nor `]`.

`Labels` admits a leading underscore in label names only via the lexer's `IDENT` rule, but the parser rejects `_`-prefixed label names at declaration (see [`language.md` § Naming conventions](language.md#naming-conventions)). Anonymous `labels` declarations are single products; sum syntax requires the named form so there is an alias to bind. In `LabelPayload`, exact bare `_` is not an inferred top-level annotation: it is the surface-only marker that reuses an earlier explicit module-local label declaration. A nested `_` remains the ordinary inference placeholder governed by its surrounding type position. A `pub op` or `pub varop` exports its operator binding for module imports via complete tagged operator import selections below (see [`language.md` § Operators — Module-local by default; `pub` exports the binding](language.md#operators)). `Equiv` enforces `N >= 2` arms at parse time. Its arms are semicolon-separated expressions; repeated semicolons are accepted and a final semicolon is ignored.

`LabelForward` binds one lowercase label spelling to one existing label family.
The target is an ordinary local or explicitly imported `LabelPath`; both sides
use braces, and neither side admits type arguments. It has no type expression,
uppercase declaration, member block, or `rec` form, and is not a `TypeRecMember`.
Visibility applies to the new label spelling. See
[`language.md` § Labels](language.md#labels).

`ValuePath` is the ordinary lexical value-path shape shared by expressions and
declaration callable targets. Its head may be a local declaration or selective
import; dotted segments may select through an explicitly imported module alias
or name a local/imported newtype member, directly or through a fully saturated
positional identity alias. A slash-separated `ModulePath` is not a `ValuePath`:
callable fields do not admit package FQNs such as `a/b.f`.
Resolution uses the same source-order classes as an ordinary path expression:
same-module ordinary and host `fn` targets, `rec` members, newtype heads, and
identity-alias heads must already be in scope. Qualifying or self-importing a
same-module target does not bypass its source-order rule.

The path in an `ElaboratorBody`'s implementation slot must resolve to a
function whose type matches the ABI selected by that slot. Arbitrary
expressions, including inline lambdas and calls, are not admitted. Every
captured item must have visibility equal to or wider than the elaborator
declaration because captures can survive in the generated runtime term. An
implementation declared in the elaborator's defining module may remain
private, and any implementation may call private helpers in its own module. A
target declared elsewhere must be ordinarily importable into the defining
module, but need not meet the elaborator's outward visibility. The
implementation and every ordinary function it calls during compile-time
execution must be declared `pure`. See [`language.md` § Elaborators are
imported, not ambient](language.md#elaborators-are-imported-not-ambient).

`ElaboratorItem` and both implementation modes are Kio-surface-only.
The Kio' item grammar has no `elab`, `impl`, or `impl(fills)` production.

`pure` is admitted only on an ordinary `FnDef`, in both Kio and Kio'. It is not admitted on `type`, `newtype`, `labels`, `elab`, `op`, `literal`, `equiv`, `host` declarations, or any `rec` group/member. `pub` and `pure` may appear in either order on a function, but each may appear at most once; `kio fmt` emits `pub pure fn`.

`OpPattern` reserves operator components whose spelling **starts with** `//`: the parser rejects any `op` whose maximal adjacent-op-token run begins with `//`, holding the entire `//…` family (`//!`, `//?`, `//=`, …) back for future syntax. This is a `starts_with("//")` **prefix** reservation. Dot-leading operator-token components have a separate spelling rule: if a component starts with `.`, it must contain at least two dots (`..` / `.+.` valid; `.`, `.>`, `.+`, `.###$` reserved). See [`language.md` § Operators](language.md#operators).

The `type` declaration is a Kio' type alias. The `literal` declaration is surface-only: it binds a lowercase name to one bare literal token and is expanded by the desugar pass at reference sites. A bare reference expands to the stored literal; `name(Type)` expands to the stored literal carrying that annotation. See [`language.md` § Type and literal aliases](language.md#type-and-literal-aliases).

### Surface parameter additions

The Kio' productions `SignatureParam` and `FnSignatureParam` admit only value parameters inside value groups: `IDENT ':' Type` for declaration signatures and `IDENT (':' Type)?` for lambda signatures. Kio surface additionally admits **product-destructuring patterns** in value-parameter position, anywhere `SignatureParam` or `FnSignatureParam` appears (fn definitions, lambda expressions, `equiv` signatures, etc.). `host fn` value parameters are the one signature site that does *not* admit a destructuring pattern — a `host fn` parameter is always a named `IDENT ':' Type` slot.

```ebnf
SignatureParam    += ParamPatternTuple                     (* bare destructuring at top *)
                   | IDENT ':' ParamPatternTuple           (* as-pattern at top *)
FnSignatureParam  += ParamPatternTuple                     (* bare destructuring at top *)
                   | IDENT ':' ParamPatternTuple           (* as-pattern at top *)

ParamPatternTuple ::= '(' ParamPatternElem (',' ParamPatternElem)* ','? ')'
ParamPatternElem  ::= IDENT (':' Type)?                    (* named slot; `: T` optional, defaults to `: _` *)
                    | '_' (':' Type)?                      (* wildcard slot; `: T` optional, defaults to `: _` *)
                    | ParamPatternTuple                    (* nested bare pattern *)
                    | IDENT ':' ParamPatternTuple          (* nested as-pattern *)
```

A bare `IDENT` or `'_'` (no `: T`) is equivalent to `IDENT : _` / `'_' : _` — the slot's type is `Type::Infer`. For an ordinary lambda literal, the typer may resolve it from the surrounding expected function type. A `match!` clause instead determines its dispatch-pattern parameter type independently, so the scrutinee and common match result do not resolve an omitted parameter type. In that position, and in other positions with no expected parameter type (top-level `fn` definitions or `equiv` signatures), an unresolved `_` slot is a type error at the same site that catches an unconstrained `.(x) { … }`.

A `ParamPatternTuple` always carries at least one `ParamPatternElem`; the unary case `(a: A)` is admissible (single-slot product) and shares its leading-`(` and per-element shape with the n-ary form.

Within a value group, `SignatureParam` / `FnSignatureParam` and `ParamPatternElem` each disambiguate from their leading token: `(` is a tuple pattern (bare top, or nested elem); `_` is a wildcard slot; an `IDENT` is followed by either `:` `Type` (regular annotation), `:` `ParamPatternTuple` (as-pattern), or (in the `FnSignatureParam` case) nothing (un-annotated). At the surrounding signature level, `[` starts a single-binder type group and `(` starts a value group. The disambiguation between `IDENT ':' Type` and `IDENT ':' ParamPatternTuple` is one-token lookahead past the `:`: a following `(` whose contents begin with `IDENT ':'` or `'_' ':'` or `(` is a `ParamPatternTuple`; anything else parses as `Type`.

The form `ParamPatternTuple ':' Type` — pinning an outer annotation onto a destructuring pattern — is **not** admissible. The pattern's slot types already constitute the product type; an outer annotation would be redundant. Parsers reject the form with a pointer to the corresponding `IDENT ':' ParamPatternTuple` (if the user meant the as-pattern) or to the bare `ParamPatternTuple` (if the user meant the destructuring alone).

This is a surface-only extension. The desugar pass eliminates every destructuring pattern at the Surface → Desugared boundary by synthesizing the outer product type from the pattern's structure, replacing each pattern-bearing param with a plain `IDENT ':' Type` slot (the user-given `IDENT` for the as-pattern, a synthetic fresh name for the bare form), and wrapping the function body in generated `let` bindings that project the outer slot with `__fst__` / `__snd__`. Wildcard slots emit no binding. Nested patterns recurse over the projected nested product. See [`language.md` § Parameter patterns](language.md#parameter-patterns).

### Surface import-clause additions

Kio surface widens the parenthesized selection list with braced label selectors
and explicitly tagged operator grammars. An ordinary name selects the value/type
namespace; `{field}` selects label syntax. Standalone `op` and `varop` selections
are ordinary identifiers; a tag introduces an operator only when followed by
its pattern or delimiter pair. Classification follows from the importing text.

```ebnf
ImportSelective  ::= 'import' ModulePath '(' ImportItem (',' ImportItem)* ')'
ImportItem       ::= IDENT | LabelImport | OperatorImport
LabelImport      ::= '{' IDENT '}'                    (* lowercase label name *)
OperatorImport   ::= 'op' OpPattern
                   | 'varop' VaropHead
```

The selection list is nonempty, even when it contains leading or trailing
commas. In canonical formatting, its opening parenthesis stays on the
introducing line immediately after the module path. Its broken layout uses the
ordinary leading-comma rule in
[style.md](style.md#import-block-ordering). An ordinary module import must have
such a list or an explicit `as` alias; builtin block imports have neither.

A fixed `OperatorImport` carries the whole `OpPattern`: every literal run, slot
kind (`_`, `__`, `___`), later slot, and lenient group. A variadic import carries
the same whitespace-separated OPEN and CLOSE as its declaration. For example:

```kio
import syntax(op _ ? _ : ___, op _ ( <| _ |> ), varop [* *]);
```

Literal runs are consumed whole. Fixed patterns admit contextual operator-token
quotation where required; variadic heads use two unquoted mirrored runs.
Fixed-pattern lenient groups are retained in the import and its formatted form.
The formatter never merges distinct complete grammars into one public spelling.

The written grammar is sufficient to parse and format the consumer without
reading its provider. Semantic resolution subsequently checks the exact named
provider's export and requires equality of the complete grammar. Callable paths,
fold mode, and optional finalizer are implementation details of that selected
export, not part of the import projection. The shorter parser dispatch/conflict
key does not replace this complete import identity (see
[language.md](language.md#operator-grammar)).

`LabelImport` braces are namespace syntax, not a label value. Its `IDENT` obeys
the lowercase label-name rule. Label and operator selectors are consumed before
Kio'; its `ImportSelective` production continues to admit ordinary identifiers
only.

Kio surface also adds a builtin block import for the complete compile-time
reflection/helper surface. It has no qualified or selective form and is not
admitted in Kio'.

```ebnf
ImportEntry += ImportComptime
ImportComptime ::= 'import' '__comptime__'
```

### Surface type additions

```ebnf
TypeAtom += '_'
```

`_` is the Kio-surface inference placeholder; the semantic rules restrict the
positions that admit it, including the post-materialization rejection of a
source occurrence beneath a structural `forall` introduced inside the same
annotation (see [`language.md` § Type system](language.md#type-system)). The
grammar does not decide that predicate. Kio' does not include this alternative. `labels`
introduces generated nominal type names, so it adds no other type grammar.
Type positions use the existing type-path production with generated names such
as `Foo` and `Foo(T)`. Lowercase labels and braced label forms are value syntax;
`{foo}`, `{foo: T}`, and `foo` as a type spelling are rejected.

### Surface expression additions

```ebnf
LiteralCall += (INT_LIT | FLOAT_LIT | STR_LIT+ | BoolLit)                    (* unannotated literal — surface only *)

ExprAtom += TupleLit | LabelValue | BangCall | BlockElabCall | PlaceholderLambda | RecCall
ExprSuffix += FieldAccess | FieldUpdate

TupleLit          ::= '(' Expr ',' Expr (',' Expr)* ','? ')'                  (* n-ary tuple; n >= 2 *)
LabelPath         ::= IDENT ('.' IDENT)*                                      (* local or qualified label path *)
LabelValue        ::= '{' (LabelValueLabel (',' LabelValueLabel)* ','?)? '}'
LabelValueLabel   ::= LabelPath ('=' Expr?)?                                  (* bare means local payload; bare `=` means `= ()` *)
FieldAccess       ::= '.?' '{' (FieldAccessLabel (',' FieldAccessLabel)* ','?)? '}'
FieldAccessLabel  ::= LabelPath
FieldUpdate       ::= '.!' '{' (FieldUpdateLabel (',' FieldUpdateLabel)* ','?)? '}'
FieldUpdateLabel  ::= LabelPath ('=' Expr?)?                                  (* bare means local payload; bare `=` means `= ()` *)

BangCall          ::= IDENT '!' CallArgList
BlockElabCall     ::= IDENT '!' BlockPrefix? NeutralBlock LabelledBlock* FinalElidedBlock?
BlockPrefix       ::= BlockValueArgs | BarePrefixExpr
BlockValueArgs    ::= '(' (Expr (',' Expr)*)? ','? ')'
BarePrefixExpr    ::= Expr                                                   (* enclosing direct brace is reserved; see below *)
LabelledBlock     ::= IDENT NeutralBlock
FinalElidedBlock  ::= IDENT BlockElabCall
NeutralBlock      ::= '{' ';'* ((NeutralBinding | Expr ';') ';'*)* Expr? '}'
NeutralBinding    ::= LetStmt | PatternLet | ExistentialLet | RowLet | NeutralBind
NeutralBind       ::= 'let' NeutralBinder '<-' Expr ';'
NeutralBinder     ::= LetBinder | '.' ParamPatternTuple
RecCall           ::= 'rec' RecCallAnnotationList? IDENT CallArgList         (* group-local callee *)
RecCallAnnotationList ::= '(' RecCallAnnotation (',' RecCallAnnotation)* ','? ')'
RecCallAnnotation ::= 'poly' | 'cont' | 'escape'
CallArgList       ::= '(' (CallArg (',' CallArg)*)? ','? ')'
NonemptyCallArgList ::= '(' CallArg (',' CallArg)* ','? ')'
                                                                             (* BangCall's IDENT is any in-scope user-defined elaborator name —
                                                                                there are no reserved elaborator keywords; see the prose above *)

DotPlaceholderIntro ::= '.' IDENT '.'                                        (* adjacent tokens; value-binding stem ending in an ASCII letter *)
PlaceholderLambda ::= DotPlaceholderIntro Block                              (* source-local numbered references belong to this stem *)
```

Every block-call head is a single value-shaped identifier with an adjacent
`!`; continuation labels are unmarked. The parser records neutral items
without looking up the elaborator or its descriptors. `product`, `thunk`,
and `sequence` interpretation and descriptor/content validation happen after
the exact elaborator declaration is selected. Empty neutral blocks and
leading, repeated, and trailing semicolons are syntactically admitted; the
selected exposure determines their meaning or rejection.

A bare prefix contains one value expression and reserves the next direct
brace for its enclosing block call. This reservation survives ordinary,
recursive, and greedy operator slots (`_`, `__`, `___`): greediness changes
operator-chain extent, not brace ownership. Parentheses and explicitly
delimited operand regions establish an inner expression boundary. Thus
`outer! left + value { body }` gives the body to `outer!`, while a nested
block operand is grouped as `outer! left + (inner! value { child }) { body }`.
An operator slot followed by its pattern's explicit continuation token can
contain the nested block before that token.

The parenthesized prefix is a value-argument list, not one tuple argument:
`outer!(a, b) { body }` has two prefixes and `outer!((a, b)) { body }` has
one. `outer!() { body }` has one unit prefix; `outer! { body }` has none.
Type-shaped argument roots are rejected; type arguments of ordinary value
expressions such as `f(A)` remain admissible. An operator-leading prefix
requires parentheses. A signed numeric token such as `-1` is a literal,
not an operator-leading prefix.

Inside the enclosing bare-prefix boundary,
`outer! convert!(value) { body }` contains a blockless `convert!` call.
Grouping the inner call together with its block makes it a nested block call:
`outer!(convert!(value) { child }) { body }`. Only the final labelled block
may elide its braces, and then its whole body is another block call. This gives
`if! condition { yes } else if! other { maybe } else { no }` its right-nested
ownership without reserving `if`, `do`, `else`, or any library name.

```ebnf
ExprAtom += PrefixOpExpr | VariadicLiteral

VariadicLiteral  ::= VaropOpen (Expr (',' Expr)*)? VaropClose
                     (* ordinary comma-list rule; CLOSE is the exact mirror of OPEN *)

PrefixOpExpr      ::= FixedOpToken+ Expr                                      (* prefix-unary operator call; the run sequence matches a registered prefix-keyspace `op` pattern *)

ExprSuffix += DotSpliceSuffix | OperatorTail
ExprPostfix += LeftCallSplice ExprSuffix*

DotSpliceSuffix   ::= ('.>' | '.>>') DotSpliceCallee NonemptyCallArgList?
DotSpliceCallee   ::= IDENT ('.' IDENT)* | IDENT '!'                          (* path; plain function or T.member; or bang-call elaborator *)
LeftCallSplice    ::= LeftSpliceCallee ('.<' | '.<<') TightCallArg
LeftSpliceCallee  ::= LeftSplicePathCallee
                   | IDENT '!' NonemptyCallArgList?
LeftSplicePathCallee ::= IDENT ('.' IDENT)* NonemptyCallArgList?
                      | '(' LeftSplicePathCallee ')'                         (* grouping is transparent *)
TightCallArg      ::= ExprAtom CallArgList*                                   (* stops before another ExprSuffix *)

OperatorTail      ::= (FixedOpToken Expr)+                                    (* infix / postfix operator call *)
```

Placeholder references use the existing single-segment value-path `IDENT` production, not a token or expression alternative. Inside `.stem. { ... }`, the stem followed by a positive decimal index is owned by that lambda unless shadowed by a body-local authored value binder. Indices have no leading zeroes and fit in `u32`. At least one owned reference is required; the highest index determines arity. Explicit lambdas retain the enclosing owner, while nested placeholder lambdas start fresh owners. Qualified paths, type positions, and dedicated bang/recursive callee positions do not participate. Ordinary UFCS callees do. See [Placeholder lambdas](language.md#placeholder-lambdas) for the complete source-local ownership rule.

The `PrefixOpExpr` and `OperatorTail` productions cover fixed prefix, infix and postfix operators, including multi-token and matched-pair patterns. Their tokens contain neither `[` nor `]`. `VariadicLiteral` instead recognizes its mirrored delimiter pair and comma-separated region boundaries from local syntax. Fixed operators inside an element still use the consumer's explicit operator scope. Operator folding resolves each use in the consumer's explicit scope and replaces it with ordinary calls before Kio'. See [`language.md` § Operators](language.md#operators) for associativity and grouping rules; variadic elements retain those ordinary expression rules.

### Surface statement additions

```ebnf
Stmt += PatternLet | ExistentialLet | RowLet

PatternLet        ::= 'let' '.' ParamPatternTuple '=' Expr ';'               (* typed unary, tuple, or as-pattern *)
ExistentialLet    ::= 'let' '.' '(' ExistsBinder+ ExistentialLetBinder ')' '=' Expr ';'
ExistentialLetBinder ::= IDENT | ParamPatternTuple                           (* existing parameter as-pattern disambiguation applies *)
RowLet            ::= 'let' '.' '(' '{' RowLetEntry (',' RowLetEntry)* ','? '}' ')' '=' Expr ';'
RowLetEntry       ::= LabelPath ('as' IDENT)?                                 (* no empty row-let list *)
```

`ExistentialLet` denotes the two-step CPS call against the underlying newtype's CPS projector — see [`language.md` § Existential type binders](language.md#existential-type-binders). Inside a neutral block, parsing retains the binding and its following items without selecting a block exposure. Its value binder is a name or a parenthesized parameter pattern under the existing parameter as-pattern disambiguation. Examples include `(left: Hidden, right: Hidden)` and `(left: _, right: _)`; an untyped opening pattern `(left, right)` is not admitted. Nested patterns and wildcard components retain the parameter-pattern rules. The existential witnesses scope over the pattern's annotations and the rest of the block, with the same escape check as a named opening.

`PatternLet` extends ordinary `let NAME = ...;` with the explicit `let .(PATTERN) = ...;` form. Typed unary `let .(x: T) = e;`, tuple `let .(x, y) = e;`, and as-pattern `let .(whole: (x: A, y: B)) = e;` bindings retain the same checking and projection semantics as parameter patterns. A unary name-only pattern formats as ordinary `let x = e;`. Tuple/as-patterns lower to a scrutinee binding and ordinary projection lets. `RowLet` evaluates its RHS once; a shorthand entry binds the last label-path segment and an `as` entry binds the explicit alias. Row and existential patterns are pure `=` bindings, including in a neutral block; they are not `<-` binders.

At statement start, `let` is structural only before a name/wildcard binder or before `.` followed by `(`. The latter commits to a rich binding pattern; it is not an expression lookahead across the parenthesized group. Thus `let(f)` remains an ordinary call, including in an expression such as `let(f) <- x` when the operator is declared. There is no keyword-free binding alias. Kio' retains only its ordinary name/wildcard `LetStmt` production.

### Notes (Kio surface)

- The surface `LiteralCall` extension makes the trailing `'(' Type ')'` annotation optional — the unannotated literal is admissible in any expression position. The three-tier resolution rule in [`language.md` § Literals](language.md#literals) decides the literal's type from context while checking the Lowered tree. Kio' requires the annotation; the Lowered → Prime substitute pass materializes the typer's recorded resolution, so the Prime artifact is fully annotated.
- A `LiteralAliasValue` accepts exactly one bare literal token. Adjacent string-literal folding applies to expression literals, not to `literal` declaration bodies; write the complete literal in one string token.
- The dot-splice forms are one syntax family. `.>` and `.>>` take a path-shaped callee or single-name bang callee on the right; `.<` and `.<<` take a path, single-name bang callee, or direct path call on the left and a tight value expression on the right. An omitted UFCS argument list is the receiver-only form. A present UFCS argument list must be `NonemptyCallArgList`: `r.>f()`, `r.>>f()`, `f().<r`, and `f().<<r` are rejected, as are their member- and bang-callee analogues. The argument list immediately adjacent to a UFCS callee belongs to that splice and cannot be reinterpreted as a direct call on the splice result. `TightCallArg` deliberately stops before another suffix: `f.<x.>g` parses as `(f.<x).>g`; use `f.<(x.>g)` when the inserted value is itself a UFCS expression.
- Dot-splice argument lists use ordinary `CallArg` parsing for every callable.
  The typer resolves which surface arguments are type arguments and which are
  values against the resolved callee type, then inserts the receiver into the
  first value slot for `.>` / `.<<` or the last value slot for `.>>` /
  `.<`. Declaration parameter groups and callee provenance are not inputs.
  Type slots are never filled by the receiver.
- A braced label-value prefix is parenthesized, as in `outer!({field = value}) { body }`.
- `rec(loop)` items and `rec name(...)` / `rec(poly, cont) name(...)` calls are surface-only. `rec(loop)` lowers to state-packet `newtype`, ordinary `fn`, and call forms before Kio'; generated packets use `rec newtype` or one minimal bare `rec { ... }` type group when their payload graph is cyclic. `rec labels` likewise lowers its generated declarations to ordinary Kio' declarations, preserving every genuine recursive component as `rec newtype` or `rec { ... }`. The capability-free Kio' forms `rec newtype` and bare `rec { ... }` are not recursive-function syntax. A recursive function call must be marked with `rec`; `poly` marks a type-changing recursive call, `cont` marks a non-tail recursive call, and `escape` is reserved — an `escape`-marked call is rejected. See [`language.md` § Recursive functions](language.md#recursive-functions).
- The parser recognizes `OperatorTail` only after parsing an `Expr`; `PrefixOpExpr` matches before parsing an `Expr` when the next op-token run begins a pattern registered in the prefix keyspace (per [`language.md` § Operators](language.md#operators)). An item's expression scope contains exactly imported operators and local `op` or `varop` declarations preceding that item, including when a function or recursive-group body is parsed lazily. A leading fixed-operator token that does not begin a registered prefix pattern is a parse error. Variadic delimiter recognition is structural; its binding is checked during resolution and lowering. An independent lexer-level carve-out applies to `-` on a numeric literal: the lexer (not the parser) consumes the `-` as part of `Literal` when (a) the `-` sits **flush against an ASCII digit** and (b) the prior token is not expression-ending. Identifiers, literals, `)`, and `}` end expressions; start-of-file and every `OpToken`, including `]`, are expression-starting. A flush `-N` after an op-token therefore fuses into a negative literal (`1 + -2` is `add(1, -2)`). A numbered placeholder reference is an ordinary identifier, so `x1 - 2` and `x1 -2` use the same expression-ending rule as other value names. Whitespace in `- 40` keeps the `-` separate. The same rule applies to float-shaped literals.
- A surface program that parses against the Kio' productions but not the Kio surface productions (i.e., one that uses no surface forms beyond what Kio' admits) is the same source either way — the surface grammar is a strict superset.
- **Parser-contextual structural peeling.** The maximal-run lexer may absorb `->`, `>`, `!`, `[`, or `]` into a longer `SymbolRun`. At a grammar position that requires one of those structural delimiters, the parser peels that leading prefix and leaves the residual as the next token. Forall binder parsing is therefore compact across kind stars and adjacent groups (`[*F]`, `[A][*F]`, `.[*F]`). The dot-splice tokens `'.>'`, `'.>>'`, `'.<'`, and `'.<<'` remain exact greedy runs. No fused-run carving happens at user-`op`, operator-expression, or operator-import positions; those consume each maximal run whole.
- **`NeutralBind` vs user-defined `<-` op.** The `<-` token remains an ordinary operator token in expressions. Only a `let`-led binder inside a neutral block treats it as a structural connective. Ordinary function and lambda blocks accept only `=` after a let binder. A selected sequence exposure admits `<-`; product and thunk exposures reject it during block validation. No context-sensitive lexing or provider lookup decides the parse.

## Package files

Several Kio file shapes live at the package root alongside any root `.kio` modules:

- `<name>.pkg.kio` — the package boundary file. Exactly one per package; stem = package name. It may carry an optional `build` block and a single `bridge { … }` block of module-path globs that selects which modules form the package's host boundary.
- `<name>.kio` — a root module. Multiple permitted; stem = root module name. A root module is an ordinary single-segment module file: it begins with `module <name>;`.
- `<local>.dep.kio` — a dependency-declaration file. One per direct dependency; stem = the dependency's **local name**. It begins with `dependency <local>;` and declares one `source { … }` block naming a local package file (`path`) or a remote git repository (`git` + `ref`). Like `<pkg>.sig.kio`, it is not a module and is kept out of module discovery (see [`package.md` § Dependency files](package.md#dependency-files)).
- `<local>.lock.kio` — a dependency lock file. One per remote `git` dependency, beside its `<local>.dep.kio`; stem = the dependency's **local name**. It begins with `lock <local>;` and declares one `resolved { … }` block pinning the `(git, ref, commit, sig)` the dependency's `ref` resolved to (`sig` is the dependency's contract-surface digest at the commit). Committed to version control; like the others it is not a module and is kept out of module discovery (see [`package.md` § Dependency files](package.md#dependency-files)).
- `<pkg>.sig.kio` — a signature changelog. At most one per package; stem = package name. It begins with `signature <pkg> v(N);` and records the package's versioned host/export contract-surface history. Generated and gated by `kio sig`; like the dependency files it is not a module and is kept out of module discovery (see [§ Signature file](#signature-file-sigkio) below and [`versioning.md`](versioning.md)).

These grammars share the lexical structure and `IDENT` / `STR_LIT` /
`ModulePath` / `Type` / `Signature` / `Block` / `TypeParamList` productions
with the module grammars above. A package file begins with `package <name>;`,
where `<name>` is the filename stem. A package is not a module, and a bare
`module ...;` directive is not admitted at the top of a package file.

### Package file (`*.pkg.kio`)

```ebnf
PackageFile       ::= PackageDecl PackageEntry*
PackageEntry      ::= BuildBlock | BridgeBlock
PackageDecl       ::= 'package' IDENT ';'                      (* IDENT = filename stem = package identity *)

BridgeBlock       ::= 'bridge' '{' SemiEntries<BridgeGlob>? '}' ';'?
BridgeGlob        ::= BridgeGlobSeg ('/' BridgeGlobSeg)*       (* a module-path glob, `/`-separated *)
BridgeGlobSeg     ::= IDENT | '*' | '**'                       (* literal segment / single-segment / subtree wildcard *)
```

Notes:

- The file must begin with a `package <name>;` directive whose `<name>` is the filename stem (for example, `app.pkg.kio` begins `package app;`). A missing directive or a name that does not match the stem is a parse error.
- After the required header, a package file admits at most one `build` block and at most one `bridge` block, in either order. Other entries, including a stray `env` / `export`, are parse errors.
- A package file declares no host items and no `import` clauses. The host boundary lives in modules: a module declares what it needs from the host with `host type` / `host fn` items and what it offers with ordinary `pub` items (see [§ Productions (Kio')](#productions-kio)). The package file only names, with module globs, which modules participate.
- `BridgeBlock` is a `;`-separated list of module-path globs spelled the same way a `ModulePath` is, extended with `*` (matches one path segment) and `**` (matches a whole subtree, zero or more segments) following shell glob semantics. It selects **modules**; from each matched module, every `pub host` item becomes a host requirement (the env — what the host must supply), every other public callable becomes a host-invocable entry (an export), and public type declarations contribute the type surface those signatures use. A plain-public `newtype` contributes its nominal type; its constructor and projector contribute independent host operations only when the corresponding member is also plain `pub`. A private or `pub(path)` outer newtype contributes no host surface regardless of its member markers. Entries keep their module namespace; there is no rename. A glob that matches no module is a build error. The module-completeness and type-closure rules that keep the exposed contract self-contained are stated in [`package.md` § The bridge block](package.md#the-bridge-block).
- `pub` is not admissible on package-file items; the package file has no items beyond `bridge` globs.

### Build block

The optional `build { ... }` block inside a package file carries the package's compilation-target configuration. It may precede or follow the optional `bridge` block after the required package header. There is no separate build-file shape.

```ebnf
BuildBlock        ::= 'build' '{' SemiRunEntries<BuildEntry> '}' ';'?
BuildEntry        ::= CacheDecl | DocsDecl | TargetBlock
CacheDecl         ::= 'cache' BlockFieldValue
DocsDecl          ::= 'docs' '{' SemiRunEntries<DocsKey> '}'
DocsKey           ::= ('md' | 'support' | 'md_out' | 'html') BlockFieldValue
TargetBlock       ::= 'target' TargetId '{' SemiRunEntries<TargetKey> '}'
TargetId          ::= IDENT ('-' IDENT)*                                       (* a bare kebab-case backend id, e.g. rust, js, kio-prime *)
TargetKey         ::= IDENT BlockFieldValue
```

Notes:

- The leading tokens `build`, `cache`, `docs`, and `target` are contextual keywords inside the build block; elsewhere in a package file they are ordinary identifiers unless another production gives them meaning.
- The `CacheDecl` is **optional** and may appear at most once, anywhere among the build entries. When **omitted** its value defaults to `()` (caching disabled); `kio fmt` inserts the `cache ()` entry, followed by `;` only when another build entry follows (see [`style.md`](style.md)). A `STR_LIT` cache value names a filesystem path (relative to the package root, or absolute); `()` opts caching out. A `cache` value that is neither a `STR_LIT` nor `()` is a parse error.
- The `DocsDecl` is **optional**, declared at most once, anywhere among the build entries. Its keys are unordered: `md` is required; `support` is repeatable; `md_out` and `html` are optional and may each appear at most once. Every `DocsKey` value is a `STR_LIT` filesystem path. See [`package.md` § Build target files](package.md#build-target-files).
- A `TargetId` is a **bare identifier** naming a registered backend (`rust`, `js`, `kio-prime`, …), not a quoted string. The hyphen-folding lets a kebab-cased backend id like `kio-prime` spell as a single id; each `-` and the following `IDENT` must be flush. Target ids must be unique within the block; duplicates are a build error (exit code `40`).
- Every target-key *value* is a `STR_LIT` unless the backend spec states otherwise. Unknown keys are a build error.

The build block's field-value grammar:

```ebnf
BlockFieldValue   ::= '(' ')' | STR_LIT | NUM_LIT | IDENT       (* . | string | number | bare word; true/false lex as IDENT *)
NUM_LIT           ::= INT_LIT | FLOAT_LIT
```

A field value is the unit literal `()`, a string literal, a number literal, or a bare word — booleans arrive as the bare words `true` / `false`, which lex as `IDENT`. Each field validates which of these kinds it accepts.

### Dependency file (`*.dep.kio`)

A dependency-declaration file names one direct cross-package dependency. It begins with a `dependency <local>;` header, where `<local>` is the filename stem (the same filename-stem coherence check `package <name>;` and `signature <name>;` run), and contains exactly one `source { … }` block naming where the dependency comes from — a local package file (`path`) or a remote git repository (`git` + `ref`).

```ebnf
DependencyFile    ::= DependencyDecl DependencyEntry*
DependencyEntry   ::= SourceBlock | RehostStmt | RetypeStmt
DependencyDecl    ::= 'dependency' IDENT ';'                    (* IDENT = filename stem = dependency local name; lowercase value name *)

SourceBlock       ::= 'source' '{' SemiRunEntries<SourceField> '}' ';'?
SourceField       ::= ('path' | 'git' | 'ref') STR_LIT
RehostStmt        ::= 'rehost' ModulePath 'to' ModulePath ';'   (* rebind the first module's host items onto the second (consumer) module *)
RetypeStmt        ::= 'retype' RetypePath 'to' RetypePath ';'   (* remap the first module's newtypes onto the second's same-named newtypes *)
RetypePath        ::= ModulePath ( '.' IDENT )?                 (* trailing `.Name` (a TYPE name) selects one newtype; same name on both sides *)
```

Notes:

- The file must begin with a `dependency <local>;` directive whose `<local>` is the filename stem (for example, `foobar.dep.kio` begins `dependency foobar;`). A missing directive or a name that does not match the stem is a parse error. The local name is a value name: it must match `_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*`.
- `dependency` is a file-shape keyword, a sibling of `package` / `module` / `signature`; it is recognized only in this leading position. A `.dep.kio` file declares no module and no `import` clauses. After its header it requires exactly one `source` block and admits zero or more `rehost` and `retype` statements (below), in any relative order. A second `source` block or any other entry is a parse error.
- A `source` block declares **exactly one** source — either a standalone `path`, or a `git` + `ref` pair with an optional `path`. All present fields may occur in any order. An empty block is a parse error.
- A standalone **`path`** key is a `STR_LIT` naming the dependency's `*.pkg.kio` package file, always spelled with its `.pkg.kio` extension. The path is **relative** (resolved against the consumer's package root) and **`/`-separated**; it may climb `../`. An absolute path (leading `/`) or a `\`-separated path is a parse error, as is a duplicate `path` or a non-string `path` value.
- The **`git`** key is a `STR_LIT` clone URL (`file://`, `https://`, `git@…`, …) and the **`ref`** key is a `STR_LIT` revision designator git resolves uniformly — a branch name, a tag, or a commit SHA. Both keys are required when either appears; a `git` without a `ref`, a `ref` without a `git`, an empty value, a duplicate, or a non-string value is a parse error.
- With `git` and `ref`, **`path`** instead selects an exact package manifest relative to the checkout. It is a unique `STR_LIT` using relative `/`-separated syntax: rooted paths, drive-qualified paths (including `C:relative`), and backslashes are parse errors. `.` and `..` components are syntactically admitted; lookup, file-kind and resolved checkout-containment checks are dependency errors, as specified in [`package.md` § Dependency files](package.md#dependency-files).
- Any key other than `path`, `git`, and `ref` is rejected as unknown.
- A **`rehost`** statement is `rehost <from> to <to>;`, where `<from>` and `<to>` are each a `/`-separated module path with no trailing item. `<from>` names a re-rooted dependency module (its leading segment is the dependency's local name); `<to>` names a consumer module that provides replacements for `<from>`'s host items. A dependency file may carry any number of `rehost` statements in any order relative to `source` and `retype`; their source order is not significant (the formatter sorts them). The rebinding semantics — rewriting `<from>`'s `host` items in place into forwarding aliases / wrappers onto `<to>` during materialization — are in [`package.md` § Dependency files](package.md#dependency-files).
- A **`retype`** statement is `retype <from> to <to>;`, where `<from>` and `<to>` are each a `/`-separated module path optionally followed by a trailing `.<Name>` selecting one `newtype` (a **TYPE** name). The trailing `.<Name>`, when present, must appear on **both** sides with the **same** name (the per-type form); omitting it on both sides remaps every `newtype` under the module. `<from>` names a re-rooted dependency module (leading segment = the dependency's local name); `<to>` names the counterpart module — another dependency's module or one of the consumer's own — holding the same-named `newtype`s. A dependency file may carry any number of `retype` statements in any order relative to `source` and `rehost`; their source order is not significant (the formatter sorts them, after the `rehost` group). Those statements must select pairwise-disjoint exact source newtypes: after expanding a module-form statement over the newtypes its source module actually declares, no exact `(module, newtype name)` may be selected again by another module-form or per-type statement, even when the second statement names the same target. The remap semantics — removing `<from>`'s matched `newtype`s and importing the same names from `<to>` during materialization, gated on a same-named, structurally-congruent counterpart — are in [`package.md` § Dependency files](package.md#dependency-files).

### Dependency lock file (`*.lock.kio`)

A dependency lock file pins the exact commit a remote `git` dependency's `ref` resolved to, plus the dependency's contract-surface digest at that commit. It is written beside the `<local>.dep.kio` it locks (same stem), is committed to version control, and begins with a `lock <local>;` header (the same filename-stem coherence check), followed by exactly one `resolved { … }` block.

```ebnf
LockFile          ::= LockDecl ResolvedBlock
LockDecl          ::= 'lock' IDENT ';'                          (* IDENT = filename stem = dependency local name; lowercase value name *)

ResolvedBlock     ::= 'resolved' '{' SemiRunEntries<ResolvedKey> '}' ';'?
ResolvedKey       ::= 'git'    STR_LIT                          (* the clone URL the lock pins *)
                    | 'ref'    STR_LIT                          (* the revision designator requested *)
                    | 'path'   STR_LIT                          (* optional repository-relative package manifest *)
                    | 'commit' STR_LIT                          (* the commit SHA the ref resolved to *)
                    | 'sig'    STR_LIT                          (* the dependency's contract-surface digest at the commit *)
```

Notes:

- The file must begin with a `lock <local>;` directive whose `<local>` is the filename stem (for example, `foobar.lock.kio` begins `lock foobar;`). A missing directive or a name that does not match the stem is a parse error. `lock` is a file-shape keyword, a sibling of `dependency`; it is recognized only in this leading position.
- The `resolved` block declares the required `git`, `ref`, `commit`, and `sig` keys and an optional `path`, each a `STR_LIT`, in any order. A duplicate key, a missing required key, a non-string value, or any other key is a parse error. `path` has the same relative-syntax rules as a Git source's selector. Anything after the `resolved` block is a parse error.
- The `sig` value is the dependency's contract-surface digest at the pinned commit (see [`versioning.md` § Git-dependency contract gate](versioning.md#git-dependency-contract-gate)); it is opaque to the grammar — any `STR_LIT`.

### Signature file (`*.sig.kio`)

A signature file records a package's versioned **contract-surface changelog** — the history of its host / export surface across sealed generations. At most one per package; stem = package name. It begins with a `signature <pkg> v(N);` header (the same filename-stem coherence check the other package-root shapes run), is generated and gated by `kio sig`, and — like the dependency files — is not a module and is kept out of module discovery.

```ebnf
SignatureFile     ::= SignatureDecl VersionBlock*
SignatureDecl     ::= 'signature' IDENT 'v' '(' INT_LIT ')' ';'   (* IDENT = filename stem; v(N) = current generation *)
```

The version-block body admits optional `with`, `breaking`, and `nonbreaking` entries in any order, at most once each. Each partition admits `add`, `modify`, and `remove` operation blocks in any order, at most once each. Ordinary nonrecursive add/modify entries remain post-elaboration Kio' declarations inside `module` sections. A complete recursive `rec { ... }` group instead appears exactly once under `with { module <path> { ... } }`, while operations name its changed members by exact non-expression `ItemRef` FQNs such as `api.A;` or `foo/bar.B;`. Canonical removals use the same FQN form; the legacy nested `module path { Name; }` removal form remains parse-compatible but is not emitted. The complete grammar and context-epoch/replay rules are specified in [`versioning.md` § Changelog grammar](versioning.md#changelog-grammar).

The one changelog-specific declaration production is the **body-less `fn` export signature**, which reuses the host-fn signature shape and is recorded without a body (a stray body is a changelog error):

```ebnf
SigExportFn       ::= ('pub' 'pure'? | 'pure' 'pub') 'fn' IDENT Signature '->' Type
                                                                  (* changelog-only; body-less export; formatter emits `pub pure fn` *)
```

`signature` is a file-shape keyword, a sibling of `package` / `module` / `dependency` / `lock`, recognized only in this leading position.

### Notes (Package files)

- Package files share `ModulePath`, `Signature`, `Type`, `Block`,
  `TypeParamList`, and the literal forms with the regular-module grammars;
  productions are not duplicated above.
- A package file admits no `module ...;` directive and no `import` clauses (see § Package file Notes above).
- `kio fmt` formats package files with the same A1 leading-comma rules as regular modules; see [`style.md`](style.md). Each `bridge` glob is formatted on its own line.

# Grammar

This file is the canonical grammar contract for Kio. The prose specs reference into this file for productions; they keep the narrative explanations of *what* each form means and *how* it desugars. This file pins *what parses*.

It is organized in three layers; the Kio' grammar is syntactically nested inside the full Kio surface:

1. **[Kio' grammar](#kio-grammar)** — the strict subset of Kio specified by [`prime.md`](prime.md); codegen runs against this subset.
2. **[Kio surface extensions](#kio-surface-extensions)** — everything the full surface ([`language.md`](language.md)) adds on top of Kio'. Each surface form desugars to Kio' (or runs an elaborator pass that elaborates to Kio') before reaching codegen; see the prose pages for the per-form desugaring.
3. **[Package files](#package-files)** — the package-root file shapes, described in [`package.md`](package.md). A package file (`<name>.pkg.kio`) is an independent file shape: it starts with `package <name>;`, admits optional `build` and `bridge { … }` blocks in either order; the latter contains module-path globs that select which modules form the package's host boundary. There is no separate build-file shape. A dependency-declaration file (`<local>.dep.kio`) is a second package-root file shape: it starts with `dependency <local>;` and names one cross-package dependency. A dependency lock file (`<local>.lock.kio`) starts with `lock <local>;` and pins the commit a remote `git` dependency's `ref` resolved to.

A program that parses against the Kio' productions is *syntactically* Kio'. A program that parses against Kio' ∪ Kio surface extensions is *syntactically* Kio. Neither claim implies well-typedness or well-resolvedness — those are separate properties checked by later compiler phases.

## Notation

`'token'` is a literal terminal. *PascalCase* names are non-terminals. `?` marks an optional element; `*` zero-or-more; `+` one-or-more. `|` separates alternatives. Whitespace, `// …` line comments, and `/// …` doc-comment lines are lexer trivia and may appear between tokens where no native adjacency rule applies; they cannot split a token or join separately written symbol words. They are not standalone productions — doc-comment attachment is described in the per-production notes below.

**Dictionary-like entries are unordered.** Where a definition body or package-root
configuration section consists of named fields or sections, those entries may
appear in any input order. Their productions repeat an entry alternative;
accompanying constraints specify required entries, duplicates and incompatible
combinations at each owner's specified validation boundary. Canonical print order
belongs to [`style.md`](style.md), not input grammar. Order within entry operands,
ordinary declaration and statement sequences, and signature history replay keeps
its separately specified meaning.

**Comma and operator runs.** Type-supply brackets and binder brackets each contain exactly one entry; they admit no commas, including edge commas. Every comma-separated production in this file — uses of `(',' X)*` — is shorthand for `(','+ X)* ','*` with an optional leading `','*` immediately after the opener: items are separated by *one or more* commas, with optional comma runs at either edge. Tuple forms collapse zero items to `()`, one item to grouping, and two or more items to Kio's right-associated binary tuple shape. Product types do not use commas; they use `&`. The type-chain productions (both the unparenthesized `TypeChainTail` and the parenthesized `'(' Type '&' Type ')'` / `'(' Type '|' Type ')'` forms) admit any number of operators in any position (leading, interior, trailing) — `A & B & C`, `(A & B & C)`, `& A & B`, and `( &&& A &&& B &&& )` all parse to the same product. Empty product chains parse as `.`, empty sum chains parse as `!`, and one-item chains parse as grouping. Adjacent `STR_LIT` tokens fold into a single literal. These rules collapse to the canonical one-separator-per-item layout at the AST level — they exist so `kio fmt` can emit the leading-comma + leading-operator multi-line layouts uniformly. See [`language.md` § Comma-separated lists and operator chains](language.md#comma-separated-lists-and-operator-chains) and [`language.md` § Literals](language.md#literals).

**Semicolon boundaries.** A complete outer braced declaration or section
accepts at most one redundant suffix `;`. Nonbraced outer entries retain their
terminator. Braces inside an entry do not change that classification:
`host type T { owned };` has a qualifier, and `type N = { X: .; };` has a
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
- `INT_LIT` — decimal digits, optionally grouped with `_` digit separators.
  Integer literals have no exponent notation: `42e0`, `42E+3`, `42e-3`, and
  `42e-0` are lexical errors. Use ordinary integer digits, or a decimal point
  when a floating-point literal was intended.
  There is no width suffix: in Kio' the mandatory `LiteralCall` annotation
  supplies the type. `42i32` lexes as `INT_LIT` then `IDENT`, not one token.
- `FLOAT_LIT` — decimal digits, a `.` followed by fractional digits, and an
  optional `e`/`E` exponent with optional `+` or `-`. Digit separators are
  admitted in the mantissa and exponent. A decimal point is required even
  with an exponent: `42.0e3` is a float, while `42e3` is rejected.
  Neither `.5` nor `1.` is a numeric literal. There is no width suffix.
- **Negative-literal carve-out.** A leading `-` flush against the first digit of an integer or float is part of the literal only when the prior token is not expression-ending. Identifiers, literals, `)`, `}`, and `]` end expressions; start-of-file and ordinary symbol words do not. Thus `a-1`, `(x)-1`, and `[a, b]-1` have binary-subtraction shape, while `[-1]` and `[* -1 *]` contain signed literals. Whitespace in `- 40` always separates the sign. This shared rule applies in full Kio and Kio'.
- `STR_LIT` — `"…"` with JSON-style escapes: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, and `\uXXXX` for code points in the Basic Multilingual Plane.
- Structural punctuation: `(` `)` `{` `}` `[` `]` `,` `;` and `.`. Dot-led native compounds `.>`, `.>>`, `.<`, `.<<`, `.?`, and `.!` retain their specified adjacent spelling; decimal dots are part of numeric tokens.
- Symbol words use one shared lexical rule in Kio and Kio': an `OpToken` is a maximal contiguous run from ``+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | :``, subject to comment and slash restrictions. Dots and brackets terminate a word. Kio' admits words only in its fixed native grammatical roles: arrows, existential delimiters, annotations, binding separators, type chains, kind stars, bottom, and module paths. A required native structural prefix may be consumed from a word in that grammatical context; user-operator matching never splits words. Structural dots and brackets keep `type A=.;`, `[*F]`, `[A][B]`, and `.[*F]` valid without consulting an operator binding.

**No user-operator productions in Kio'.** Kio' shares full Kio's lexical rules and structural delimiters, but has no fixed or variadic declaration, tagged operator import, or operator expression production. Native parsing cannot create such a production.

**Contextual keywords (Kio').** The words `module`, `import`, `as`, `pub`, `pure`, `type`, `fn`, `rec`, `let`, `mk`, `get`, `host`, and the magic phrase `__intrinsics__` lex as ordinary `IDENT` tokens. They are recognized as keywords *only* in the grammatical positions where the productions below show them as literal terminals; in any other position the same spelling is an identifier. `pure` is a modifier only on an ordinary `fn` declaration. `rec` marks either one self-recursive nominal declaration or one mutually recursive type group; it takes no runtime capability in Kio'. `host` is a host-declaration modifier only when immediately followed by `type` or `fn` (optionally with a redundant `pub` between them); elsewhere `host` is an ordinary identifier, so existing code that uses `host` as a name keeps parsing. Package-boundary words such as `package` and `bridge` are recognized only inside the package shapes that admit them. (`file` is not a Kio grammar keyword; it is a kiodoc fence attribute — see [`kiodoc.md`](kiodoc.md).)

**Comment markers require trailing whitespace.** A comment marker — `//` (line comment) or `///` (doc-comment line) — must be followed immediately by whitespace, end-of-line, or end-of-file. A non-whitespace character flush against the marker is a lex error: `//foo`, `///bar`, and `//=` are all rejected, with no exception for alphanumeric or underscore characters. In particular a fourth slash (`////`, `////////`) is rejected — four or more slashes are **not** a section ruler. This reserves the whole `//…` operator family (`//!`, `//?`, `//=`, …) for future syntax (see [§ Lexical structure (Kio surface)](#lexical-structure-kio-surface) for the matching `op`-pattern reservation).

**Doc-comment lines (`///`).** A line whose first non-whitespace characters are `///` followed by whitespace, end-of-line, or end-of-file is a doc-comment line. It is lexer trivia (like `//`) but carries its payload to the parser. Doc-comment lines attach to the immediately following top-level declaration (see the `DocBlock?` prefix of `Item` in [§ Productions (Kio')](#productions-kio)); a `///` block before an `import` clause is a parse error.

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
ImportSelective   ::= 'import' ModulePath '(' NamedImport (',' NamedImport)* ')'
NamedImport       ::= IDENT ('as' IDENT)? (* destination preserves identifier class *)
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
NewtypeAfterVis   ::= 'type' '{' ';'? NewtypeEntry ';'? '}'
NewtypeEntry      ::= IDENT TypeParamList? ExistsBinder* ':' Type NewtypeBody
NewtypeBody       ::= '{' SemiRunEntries<NewtypeMember> '}'  (* exactly one mk and one get *)
NewtypeMember     ::= Vis? 'mk' | Vis? 'get'

TypeRecGroup      ::= 'rec' '{' SemiEntries<TypeRecMember> '}'  (* at least two members *)
TypeRecMember     ::= DocBlock? (TypeAliasBody | Newtype)

HostType          ::= HostTypeBody ';'
HostTypeBody      ::= 'pub'? 'host' 'pub'? 'type' IDENT TypeParamList? RoleAnnotation? OwnedBlock?
HostFn            ::= HostFnBody ';'
HostFnBody        ::= 'pub'? 'host' 'pub'? 'fn' IDENT ValueSignature ('->' Type)?
RoleAnnotation    ::= 'role' '(' IDENT ')'    (* IDENT ∈ i8…i128 / u8…u128 / f32 / f64 / bool / str *)
OwnedBlock        ::= '{' 'owned' '}'

TypeParamList     ::= TypeBinderGroup+
Signature         ::= SignatureGroup+                     (* ordinary declaration; type-only groups admitted *)
ValueSignature    ::= SignatureGroup* ValueParamGroup SignatureGroup*
FnSignature       ::= FnSignatureGroup+                   (* lambda; type-only groups admitted *)
SignatureGroup    ::= TypeBinderGroup | ValueParamGroup
FnSignatureGroup  ::= TypeBinderGroup | FnValueParamGroup
ValueParamGroup   ::= '(' SignatureParamList? ')'
FnValueParamGroup ::= '(' FnSignatureParamList? ')'
SignatureParamList ::= SignatureParam (',' SignatureParam)*
SignatureParam    ::= IDENT ':' Type                      (* value parameter *)
FnSignatureParamList ::= FnSignatureParam (',' FnSignatureParam)*
FnSignatureParam  ::= IDENT (':' Type)?                   (* value parameter; annotation optional *)
TypeBinderGroup   ::= '[' TypeBinder ']'
TypeBinder        ::= '*'* IDENT                          (* kind-annotated binder; n leading stars = n arrows *)

Type              ::= TypeArrow
TypeArrow         ::= TypeBinderGroup+ TypeArrow          (* forall, prefix-only *)
                    | TypeArrowParam '->' TypeArrowResult (* function type; right-associative *)
                    | TypeChain
TypeArrowParam    ::= TypeAtom                            (* product/sum LHS requires parentheses *)
TypeArrowResult   ::= TypeBinderGroup+ TypeArrow
                    | TypeArrowParam ('->' TypeArrowResult)? (* direct product/sum result requires parentheses *)
TypeChain         ::= TypeAtom TypeChainTail?
TypeChainTail     ::= ('&' TypeAtom)+                     (* unparenthesized product chain *)
                    | ('|' TypeAtom)+                     (* unparenthesized sum chain     *)
TypeAtom          ::= '!'                                 (* bottom             *)
                    | '.'                                 (* unit               *)
                    | '(' Type ')'                        (* grouping           *)
                    | '(' Type ('|' Type)+ ')'            (* parens sum chain   *)
                    | '(' Type ('&' Type)+ ')'            (* parens product     *)
                    | TypePath
TypePath          ::= TypeName TypeArgList*
                    | ModulePath '.' TypeName TypeArgList*
TypeName          ::= IDENT                               (* type-word shape, with user or reserved underscore prefix *)
ExistsBinder      ::= '<' IDENT '>'
TypeArgList       ::= '[' Type ']'

Block             ::= '{' BlockBody '}'
BlockBody         ::= ';'* (Stmt ';'*)* Expr ';'*
Stmt              ::= LetStmt | ExprStmt
LetStmt           ::= 'let' LetBinder '=' Expr ';'
LetBinder         ::= IDENT | '_'
ExprStmt          ::= Expr ';'
Expr              ::= ExprPostfix
ExprPostfix       ::= ExprAtom ExprSuffix*
ExprSuffix        ::= ValueApplication | TypeApplication
ValueApplication  ::= '(' (Expr (',' Expr)*)? ')'
TypeApplication   ::= '[' Type ']'                       (* exactly one type *)
ExprAtom          ::= LiteralCall
                    | '(' ')'                             (* unit value       *)
                    | '(' Expr ')'                        (* grouping         *)
                    | ValuePath
                    | FnExpr
LiteralCall       ::= (INT_LIT | FLOAT_LIT | STR_LIT+ | BoolLit) '[' Type ']'
BoolLit           ::= '.' ('t' | 'f')                     (* token sequence: `.` plus identifier `t` / `f` *)
FnExpr            ::= '.' FnSignature ('->' Type)? Block
```

### Notes (Kio')

- `(` introduces several productions in both type and expression positions; in type position it is grouping unless the contents form a parenthesized `&` / `|` chain. A grouped type may be followed by `->`, so `(A) -> R` is the same function type as `A -> R`, and `(A & B) -> R` uses the grouped product as the function domain. Type position has no comma-product production and empty parentheses are not a type: `(A, B)` and `()` are not type expressions. The grammar above is unambiguous given one-token lookahead after the opening `(` — extended to also recognize a leading `&` / `|` as a chain opener (the formatter's multi-line layout).
- Type-binder groups inside type expressions are prefix-only. `[A] A -> R` and `[A] (A & B) -> R` are valid forall-prefixed types; `[A] -> R` is rejected because the binder run has no body type.
- Mixing `&` and `|` in one parenthesized chain remains a parse error — `(A & B | C)` is ill-formed; users wanting both must explicitly parenthesize as `(A & (B | C))` or `((A & B) | C)`. Bare unary arrows are right-associative (`A -> B -> C` is `A -> (B -> C)`), but their left side is atomic: a product/sum parameter must be parenthesized (`(A & B) -> C`, not `A & B -> C`; `(A | B) -> C`, not `A | B -> C`). A direct product/sum result is likewise grouped (`A -> (B & C)` or `A -> (B | C)`); bare result chains reject. A named function or lambda's own return annotation uses `Type` and may retain a bare chain, but an arrow nested inside it still uses `TypeArrowResult`. Forall prefixes retain their own full body scope. Mixed chains still reject: `A & B | C` and `A | B & C` are ill-formed without explicit grouping. Leading, trailing, and repeated arrow-token runs are not accepted.
- `ValuePath` is the lexical path shape for value expressions. A bare head is
  local or selectively imported; dotted segments select through an explicitly
  imported module alias or name a local/imported newtype member, directly or
  through a fully saturated positional identity alias. The dots are part of
  the path, not general member access on an arbitrary expression. A
  slash-separated `ModulePath` is not a value expression; importing the item
  or module establishes the cross-module binding first.
- Value applications and type applications have distinct delimiters. One `ValueApplication` supplies one right-associated product to one `Function` layer; it cannot continue into a returned function. One component is that value, no components supply Unit, and several components form the ordinary product. `f(a, b)` and `f((a, b))` therefore agree. A `TypeApplication` supplies one type to an exposed universal binder and never inserts Unit. Kio' writes every universal argument explicitly. Surface Kio may omit them or use whole-slot `_` inference as specified below.
- A square pair after a completed expression is a type-application suffix, regardless of whitespace or comments. A square pair at expression start retains its Surface variadic-frame role. Thus `f[A]` and `f [A]` agree, `f[a]` rejects a value in type position, and `f([a])` passes a variadic value. Parsing never retries the frame interpretation after an invalid type argument. `f[]` is rejected. The full type grammar applies inside a suffix, including nested forall types such as `f[[A] A -> A]`.
- `LiteralCall` is a literal token or parser-recognized literal token sequence with a mandatory trailing `'[' Type ']'` annotation — `42[I32]`, `3.14[F64]`, `"hi"[String]`, `.t[Bool]`. The annotation pins the literal's role-bearing host type. See [`language.md` § Literals](language.md#literals); Kio' admits no unannotated-literal form. This is a literal annotation, distinct from the value-only application list; its result is one literal node carrying the annotation. The Kio-surface extensions section below re-introduces the unannotated form additively for the full surface language, where the literal-type selection rule in [`language.md` § Literals](language.md#literals) uses an explicit annotation or concrete contextual inference.
- A named type path collects its consecutive singleton argument groups before kind and exact-arity checks. `Pair[A][B]` supplies two arguments to the one named head; it does not validate an intermediate undersaturated alias. Existing nominal partial application and transparent-alias exact saturation rules remain unchanged. Grouped types still use parentheses, while arbitrary type expressions do not acquire a new postfix-application production.
- **Higher-kinded type application** is the ordinary `TypePath` shape applied to a kind-`*→*`-or-higher binder or newtype head: `F[A]` where `[*F]` is a kind-`*→*` binder, `Either[String]` where `Either` is an arity-2 newtype. Both parse to one `Type::Path { segments, args }`; there is no dedicated application node and no `__App__` keyword. The kind discipline (§ Kind grammar) decides admissibility — a kind-`*` head applied to an argument is a kind error, surfaced by semantic kind checking rather than the parser. A parametric transparent alias uses the same parsed shape but is well-formed only when every parameter declared by that alias has an argument; underapplication cannot retain the alias's own binders. See [`prime.md` § Kio' as Church-style fragment of System F-ω](prime.md#kio-as-church-style-fragment-of-system-f-ω) and [`language.md` § Higher-kinded types](language.md#higher-kinded-types).
- `pub` is a contextual keyword only in two positions: leading an `Item` and leading a `NewtypeMember`. Elsewhere the same spelling is an `IDENT`.
- A `NewtypeBody` holds an **unordered set** of members: exactly one `mk` and exactly one `get`, in either order. Both must be present and neither may be duplicated; the names are fixed and the order carries no meaning. `kio fmt` canonicalizes the pair to `mk` then `get` (see [`style.md`](style.md)).
- `RecursiveNewtype` is the singleton recursive-data spelling. Its `rec` follows visibility (`pub rec type { N: Payload { mk; get; }; }`); a self-recursive newtype requires it, while a marker on an acyclic newtype is rejected as redundant. An ordinary `Newtype` has no own-head or later-declaration scope.
- `TypeRecGroup` is capability-free and has no group-level visibility. It contains at least two ordinary alias or explicit singleton nominal `type` declarations, with visibility and doc comments attached to each member. The complete member heads are in scope throughout the group. Static validation requires the written group to be exactly one genuinely mutual cyclic component and requires every alias cycle to cross a `newtype` boundary; a one-member, acyclic, multi-component, or alias-only group is rejected. A member does not repeat the singleton `rec` marker.
- The keyword `'fn'` leads only item declarations. Ordinary declarations and dot-led anonymous functions require at least one type or value group: `fn value[A] { … }` and `.[A] { … }` are admitted alongside `fn value() { … }` and `.() { … }`. No value group is implied by a type-only signature. `host fn`, `RecFnDef`, and a written `Equiv` signature instead use `ValueSignature`, which requires at least one value group. Expression position does not reserve `fn`.
- A module's `import` clauses all precede its items in one flat run (`ModuleDecl Import* Item*`); there is no `import`-block split. `import m(X);` creates a `→ m` import edge wherever it sits. See the module-cycle rule in [`language.md` § Module system](language.md#module-system).
- `HostType` and `HostFn` are opaque, signature-only **host declarations** — a `host type` is a nominal type the host supplies, a `host fn` a function whose body the host supplies. They are ordinary module items (not a special block), are always public, and obey ordinary lexical scoping: another module reaches a `host` item through a plain `import`, exactly like any other `pub` item. `host` is a contextual keyword (see § Contextual keywords above). A `host type`'s optional `role(...)` annotation records its literal-admission metadata (see [`language.md` § Literals](language.md#literals)); `role(bool)` also participates in [checked Boolean specialization](language.md#conditionals). `TypeParamList` and `RoleAnnotation` are individually syntactic components, but host type parameters must be unstarred kind-`*` binders and are semantically mutually exclusive with `RoleAnnotation`: a role-bearing `host type` has no type parameters. Violating either restriction is a type error. The optional `{ owned }` block remains accepted and formatted for source compatibility, but selects no alternate representation: the Rust backend renders every `role(str)` facade in owned form at every occurrence (see [`backends/rust.md`](backends/rust.md)). A `host fn`'s value parameters carry the same named, annotated `SignatureParam` shape as an ordinary `FnDef`, and an omitted `-> Type` result defaults to `.`. This is a fixed Unit default, not inference. `pub host` / `host pub` are admitted but redundant — there is no private host item. Host items are part of Kio': they appear in the boundary AST and reduce as opaque primitives. Which modules' host items reach the host is selected by the package file's `bridge` block (see [§ Package files](#package-files)).

The `IS_KIO_PRIME` corpus marker (see [`test-data/README.md`](../test-data/README.md)) asserts that every regular-module file in a case parses against the Kio' productions above. The corpus check `ci/checks/orchestrators/golden-tests.sh` verifies acceptance via the standalone `kio-prime-check` tool under [`ci/infra/kio-prime-check-rs/`](../ci/infra/kio-prime-check-rs/).

### Kind grammar

Every type binder and every type constructor has a **kind** — the type-level arity that decides how many type-arguments it accepts. The kind language is a right-associative chain over a single base kind:

```ebnf
Kind              ::= '*'                                 (* the kind of ordinary (saturated) types *)
                    | '*' '→' Kind                        (* a type-level function, right-associative *)
```

So `*` is the kind of a fully-applied type (`String`, `(A | B)`, `Box[A]`), `*→*` is the kind of an arity-1 constructor (`Box`, `List`, a `[*F]` binder), `*→*→*` is the kind of an arity-2 constructor (`Either`, `Result`, a `[**G]` binder), and so on. The chain is right-associative: `*→*→*` parses as `*→(*→*)`. There are **no kind variables** and **no higher-order kinds** — the domain of every arrow is `*`, never another arrow. Each kind is a finite chain, and equality is decided by comparing those chains structurally (see [`prime.md` § Kio' as Church-style fragment of System F-ω](prime.md#kio-as-church-style-fragment-of-system-f-ω)).

The **binder-annotation surface** encodes a kind as a run of leading `*` characters before the binder name inside a `TypeBinderGroup`. The star count `n` is the number of arrows:

| Surface | Kind | Meaning |
|---|---|---|
| `[A]` | `*` | ordinary type-parameter (the default; no stars) |
| `[*F]` | `*→*` | arity-1 type constructor |
| `[**G]` | `*→*→*` | arity-2 type constructor |
| `[***H]` | `*→*→*→*` | arity-3 type constructor |

A newtype's kind is read from its declared parameter list, not annotated separately. Each parameter carries its own kind (the default `*`, or a starred annotation), and the newtype accepts one argument per parameter, checked against that parameter's kind. An ordinary arity-`n` newtype whose parameters are all kind-`*` (`List[A]`, `Either[E][A]`) accepts `n` kind-`*` arguments and saturates to kind `*`. A *dictionary* newtype takes a higher-kinded parameter — `Monad[*F]` accepts one kind-`*→*` argument (a one-argument type constructor) and saturates to `*` — so `Monad[Maybe]` and `Monad[Either[String]]` check the argument's kind (`*→*`) against the `[*F]` parameter. (This per-parameter check is the newtype's signature; it is *not* a higher-order kind in the `κ` language above — a binder still cannot be annotated `[(*→*)→* …]`.) **Kind annotations are part of a type's public signature** — they carry unchanged across package boundaries; see [`package.md`](package.md). Kinds are never inferred: the user writes the stars at every higher-kinded binder, and a binder with no stars is kind `*`.

**Type application consumes one parameter per argument, matching kinds.** A binder or newtype head whose parameters are `*→*→*`-shaped (`Either[E][A]`) applied to one argument (`Either[String]`) leaves the remaining parameter, so the partial application has kind `*→*`; applied to two (`Either[String][ I32]`) it saturates to `*`. A parametric transparent alias instead requires exactly one argument per declared parameter: both its bare name and every undersaturated application are type errors. Applying a kind-`*` head to any argument, supplying more arguments than parameters, or supplying an argument whose kind doesn't match the parameter's kind, is a kind error.

## Kio surface extensions

The full Kio surface language extends Kio' with additional tokens, additional contextual keywords, and additional productions on existing non-terminals (plus a few new top-level item shapes). Everything in [§ Kio' grammar](#kio-grammar) carries through unchanged; this section spells out what's *additive*. The `+=` notation below means "add these alternatives to the named production from the Kio' grammar"; `::=` introduces a new production.

Each surface form's prose meaning (what it means, how it desugars) lives in [`language.md`](language.md); the productions here pin *what parses*. For the macro call mechanism — how a direct local named call resolves to a user-defined macro — see [`language.md` § Macros are imported, not ambient](language.md#macros-are-imported-not-ambient); the reference coercion palettes' per-form semantics are documented as case studies in [`docs/poc/optics.md`](../docs/poc/optics.md) and [`docs/poc/macros.md`](../docs/poc/macros.md).

### Lexical structure (Kio surface)

Kio surface admits everything Kio' admits plus:

- `OpToken` — the shared maximal symbol-word class over ``+ - * / % ^ ~ ? @ # $ \ ' ` < > = ! & | :``. Whitespace, identifiers, digits, dots, brackets, and other structural punctuation end a word. Each fixed component is one complete word; `!?` and `! ?` are different sequences. Dots and brackets are not operator-word characters. See [`language.md` § Operators](language.md#operators).
- Placeholder stems and numbered references are ordinary `IDENT` tokens. The adjacent `.stem.` introduction establishes source-local ownership; no operator-character marker or indexed-token fusion participates.

The symbols `< > [ ] = ! : . / -> .? .! .> .>> .< .<<` have fixed native roles in their grammatical contexts. Square brackets and dots are structural boundaries; their roles do not depend on imported operators. Native arrows, existential delimiters, kind stars, and type-chain separators retain their required structural-prefix handling. Fixed-operator matching consumes complete ordinary words; variadic framing instead uses the written square pair and its mirrored marker.

### Contextual keywords (Kio surface)

In addition to Kio''s contextual keywords, Kio surface recognizes:

- `trailing`, `product`, `thunk`, `sequence` — trailing-block descriptor entries in a macro declaration.
- `rec` — the recursive-declaration marker. Explicit singleton `rec type { N: Payload { mk; get; }; }` and bare `rec { ... }` retain their Kio' meanings; compact `rec type` declarations and `rec(loop)` are surface additions.
- `recur` — the surface recursive-call introducer, optionally followed by call annotations and then a group-local callee. Outside that grammatical position it is an ordinary identifier.
- `type`, `literal`, `macro`, `op`, `varop`, `equiv` — top-level item leads (after declaration modifiers where admitted). A singleton recursive declaration admits leading visibility (`pub rec type` or `pub rec(loop) fn`); either braced `rec` group carries no visibility, so visibility stays on each member.
- `__comptime__` — reserved import target, recognized only inside `import __comptime__;` (see [§ Surface import-clause additions](#surface-import-clause-additions)). Never a regular module name. Surface-only — it supplies the compile-time reflection API used by macro implementations.

Macro calls use ordinary direct call syntax. A scoped `macro` selector or an
earlier same-module declaration identifies the macro through ordinary lexical
ownership; the parser does not discover macros by inspecting provider contents.
See [Macros are imported, not ambient](language.md#macros-are-imported-not-ambient).

### Surface item additions

```ebnf
Item += NominalDeclaration | RecursiveNominalDeclaration | LiteralAlias
      | (MacroItem | Op | VariadicOperator | Equiv | RecItem) ';'?

NominalDeclaration ::= Vis? NominalDeclarationBody
RecursiveNominalDeclaration ::= Vis? 'rec' NominalDeclarationBody
NominalDeclarationBody ::= 'type' NominalGroup ';'?
                        | 'type' IDENT TypeParamList? '=' NominalRhs ';'
NominalGroup   ::= '{' SemiEntries<NominalEntry> '}'
NominalEntry   ::= IDENT TypeParamList? ExistsBinder* ':' NominalPayload NewtypeBody?
NominalPayload ::= Type                                  (* a complete payload cannot be `_` *)
NominalRhs     ::= NominalOperand NominalChainTail?
NominalChainTail ::= ('&' NominalOperand)+ | ('|' NominalOperand)+
NominalOperand ::= NominalGroup | '(' NominalRhs ')' | TypeAtom

LiteralAlias   ::= Vis? 'literal' IDENT '=' LiteralAliasValue ';'           (* lowercase name only *)
LiteralAliasValue ::= INT_LIT | FLOAT_LIT | STR_LIT | BoolLit

MacroItem ::= Vis? 'macro' IDENT ':' Type MacroBody
MacroBody ::= '{' SemiEntries<MacroEntry> '}'
MacroEntry ::= MacroCaptures | MacroImpl | TrailingBlockDecl
MacroImpl ::= 'impl' GroupedValuePath
                 | 'impl' '(' 'fills' ')' GroupedValuePath
MacroCaptures ::= 'captures' (GroupedCapturePath | '(' GroupedCapturePath (',' GroupedCapturePath)* ','? ')')
TrailingBlockDecl ::= 'trailing' BlockExposure IDENT?
BlockExposure  ::= 'product' | 'thunk' | 'sequence'
CapturePath    ::= IDENT ('.' IDENT)*
GroupedCapturePath ::= CapturePath | '(' GroupedCapturePath ')'
GroupedValuePath ::= ValuePath | '(' GroupedValuePath ')'
OptionalCallable ::= ValuePath | '(' ')' | '(' OptionalCallable ')'

RecItem        ::= Vis? 'rec' '(' GroupedValuePath ')' RecFnDef                 (* exact one-member braced expansion; at most one visibility total — before `rec` (fmt-canonical) or on the fn *)
                 | 'rec' '(' GroupedValuePath ')' '{' SemiEntries<RecFnDef> '}' (* grouping braces carry no visibility; each member does *)
RecFnDef       ::= Vis? 'fn' IDENT ValueSignature ('->' Type)? Block

TypeRecMember += DocBlock? Vis? 'type' NominalGroup
               | DocBlock? Vis? 'type' IDENT TypeParamList? '=' NominalRhs

Op             ::= Vis? 'op' OpBody
OpBody         ::= OpPattern '{' SemiEntries<OpEntry> '}'                 (* fixed arity only *)
OpEntry        ::= 'impl' GroupedValuePath                               (* exactly once *)
OpPattern      ::= OpSlot? OpWord (OpSlot OpWord)* OpSlot?
                    (* at least one slot; structural slot restrictions below *)
OpSlot         ::= '_' | '__' | '___'                 (* atom / self-chain / any-chain *)
OpWord         ::= FixedOpToken | '(' FixedOpToken ')' (* quotation, not a slot group *)
FixedOpToken   ::= OpToken                            (* no dots or brackets; bare '=' requires quotation *)
VaropHead      ::= '[' ']' | '[' Marker ReverseMarker ']'
Marker         ::= OpToken
ReverseMarker  ::= OpToken                            (* exact character reversal of Marker *)
                    (* a legal fused empty pair is split at its mirrored midpoint *)
VariadicOperator ::= Vis? 'varop' VaropHead VariadicBody
VariadicBody      ::= '{' SemiEntries<VariadicEntry> '}'
VariadicEntry     ::= VariadicPrimary | 'finalize' OptionalCallable
VariadicPrimary   ::= 'foldl' GroupedValuePath GroupedValuePath      (* step, nullary base *)
                   | 'foldr' GroupedValuePath GroupedValuePath      (* step, nullary base *)
                   | 'foldl1' GroupedValuePath GroupedValuePath     (* step, unary first-element seed *)
                   | 'foldr1' GroupedValuePath GroupedValuePath     (* step, unary last-element seed *)

Equiv             ::= 'equiv' IDENT ValueSignature?
                    '{' ';'* Expr (';'+ Expr)+ ';'* '}'                       (* N >= 2 arms *)
```

The fixed operator body has exactly one `impl` entry. The macro body has
exactly one implementation entry, at most one captures entry, and zero or more
trailing-block entries. Entries may be interleaved in any order; the relative
order of trailing-block entries declares block order. The first descriptor is
unlabelled; every subsequent descriptor has a distinct value-shaped label.
The variadic body has exactly one primary clause and at most one finalizer,
in either order; different primary modes are mutually exclusive.
Grouping a named reference is transparent and may repeat. Required callable
fields still require a name; `finalize ()` explicitly selects the same absence
as omitting that optional field. An explicit absence still counts as a field
for duplicate detection. Captures retain their nonempty-list requirement.
Immediately after `impl`, exact `(fills)` is the scheduling marker only when
another callable target follows; `impl (fills)` alone names the ordinary
callable `fills`. Grouping introduces no calls, lambdas, or evaluated values.
`foldl` and `foldr` accept an empty literal and invoke a
nullary base; `foldl1` and `foldr1` require an element and seed from the first
or last element respectively. Each element is one ordinary expression. The
step receives the accumulator first for left modes and last for right modes;
an optional unary finalizer runs once and may change the result type. The full
expansions are specified in [language.md](language.md#operators).

`VaropHead` is one structural square pair. It contains either nothing or one marker and its character reversal; a legal fused empty pair is split at its unique mirrored midpoint. `[]`, `[**]`, and `[* *]` select the bare, marker-`*`, and marker-`*` forms respectively. Commas, slots, nested bracket pairs, dots, and symbol words outside the square edges are rejected in a head. Canonical spelling is `[]` or a spaced pair such as `[* *]`.

Compact entries use ordinary type names, including `_Name`, and introduce those exact nominal identities. An anonymous group declares entries without an aggregate alias. A named declaration RHS composes compact groups and ordinary type operands through products, sums and parentheses; it contains at least one compact group. Each group contributes the right-associated product of its entry types. The ordinary same-operator chain rules apply, and mixing `&` with `|` requires parentheses. Parenthesized function and polymorphic types and ordinary applied types can be operands. Compact groups are declaration syntax: they cannot occur inside a type argument, arrow, internal binder, payload, function signature or other ordinary `Type` position.

Each entry has its own universal and existential header; an owner's parameters do not become implicit entry parameters. Omitting an entry's member body supplies `mk` and `get` with the enclosing declaration's visibility. A written body requires both fixed roles and gives each its own independent visibility marker, private when omitted. Kio' admits only the anonymous singleton form with a written body; multiple entries, omitted bodies and declaration-bearing alias RHSs are surface forms.

Each compact entry declares a new nominal with an explicit payload type; `_` cannot be its complete payload. Existing types are ordinary RHS operands, such as `type Result = { Ok: .; } | Error;` or `type Pair[A] = Existing[A] & { Extra: .; };`. Nested `_` remains the ordinary inference placeholder governed by its surrounding type position. A named owner and an entry cannot introduce the same name.

A compact `rec type` introduces exactly its written owner and entries into one atomic recursive scope. Positivity, nominal grounding and minimal recursive partitioning follow the existing recursive-data rules. A recursive alias without a nominal boundary remains rejected. Bare `rec { ... }` groups retain their minimum of two declarations.

A `pub op` or `pub varop` exports its operator binding for module imports via complete tagged operator import selections below (see [`language.md` § Operators — Module-local by default; `pub` exports the binding](language.md#operators)). `Equiv` enforces `N >= 2` arms at parse time. Its arms are semicolon-separated expressions; repeated semicolons are accepted and a final semicolon is ignored.

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

The path in a `MacroBody`'s implementation slot must resolve to a
function whose type matches the ABI selected by that slot. Arbitrary
expressions, including inline lambdas and calls, are not admitted. Every
captured item must have visibility equal to or wider than the macro
declaration because captures can survive in the generated runtime term. An
implementation declared in the macro's defining module may remain
private, and any implementation may call private helpers in its own module. A
target declared elsewhere must be ordinarily importable into the defining
module, but need not meet the macro's outward visibility. The
implementation and every ordinary function it calls during compile-time
execution must be declared `pure`. See [`language.md` § Macros are
imported, not ambient](language.md#macros-are-imported-not-ambient).

`MacroItem` and both implementation modes are Kio-surface-only.
The Kio' item grammar has no `macro`, `impl`, or `impl(fills)` production.

`pure` is admitted only on an ordinary `FnDef`, in both Kio and Kio'. It is not admitted on `type`, `macro`, `op`, `literal`, `equiv`, `host` declarations, or any `rec` group/member. `pub` and `pure` may appear in either order on a function, but each may appear at most once; `kio fmt` emits `pub pure fn`.

`OpPattern` requires alternating single words and value slots, with at least one of each. Adjacent slots, adjacent words, zero-slot patterns, and pattern parentheses containing slots are rejected. Arbitrary numbers of `_`, `__`, and `___` are admitted subject to [`language.md` § Operators](language.md#operators): every `__` must be useful, and an operand-ending non-prefix pattern cannot have chain permission at both exposed ends. Dots and square brackets are forbidden in quoted and unquoted words. A word contains at most one `/`; comment spellings do not become operator words.

The `type` declaration is a Kio' type alias. The `literal` declaration is surface-only: it binds a lowercase name to one bare literal token and is expanded by the desugar pass at reference sites. A bare reference expands to the stored literal; `name[Type]` expands to the stored literal carrying that annotation. See [`language.md` § Type and literal aliases](language.md#type-and-literal-aliases).

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

A bare `IDENT` or `'_'` (no `: T`) is equivalent to `IDENT : _` / `'_' : _` — the slot's type is `Type::Infer`. For an ordinary lambda literal, the typer may resolve it from the surrounding expected function type. A `match` clause instead determines its dispatch-pattern parameter type independently, so the scrutinee and common match result do not resolve an omitted parameter type. In that position, and in other positions with no expected parameter type (top-level `fn` definitions or `equiv` signatures), an unresolved `_` slot is a type error at the same site that catches an unconstrained `.(x) { … }`.

A `ParamPatternTuple` always carries at least one `ParamPatternElem`; the unary case `(a: A)` is admissible (single-slot product) and shares its leading-`(` and per-element shape with the n-ary form.

Within a value group, `SignatureParam` / `FnSignatureParam` and `ParamPatternElem` each disambiguate from their leading token: `(` is a tuple pattern (bare top, or nested elem); `_` is a wildcard slot; an `IDENT` is followed by either `:` `Type` (regular annotation), `:` `ParamPatternTuple` (as-pattern), or (in the `FnSignatureParam` case) nothing (un-annotated). At the surrounding signature level, `[` starts a type-binder group and `(` starts a value group. A type-binder group contains exactly one binder. Multiple binders use adjacent groups; empty groups and comma-separated binder lists are rejected. The disambiguation between `IDENT ':' Type` and `IDENT ':' ParamPatternTuple` is one-token lookahead past the `:`: a following `(` whose contents begin with `IDENT ':'` or `'_' ':'` or `(` is a `ParamPatternTuple`; anything else parses as `Type`.

The form `ParamPatternTuple ':' Type` — pinning an outer annotation onto a destructuring pattern — is **not** admissible. The pattern's slot types already constitute the product type; an outer annotation would be redundant. Parsers reject the form with a pointer to the corresponding `IDENT ':' ParamPatternTuple` (if the user meant the as-pattern) or to the bare `ParamPatternTuple` (if the user meant the destructuring alone).

This is a surface-only extension. The desugar pass eliminates every destructuring pattern at the Surface → Desugared boundary by synthesizing the outer product type from the pattern's structure, replacing each pattern-bearing param with a plain `IDENT ':' Type` slot (the user-given `IDENT` for the as-pattern, a synthetic fresh name for the bare form), and wrapping the function body in generated `let` bindings that project the outer slot with `__fst__` / `__snd__`. Wildcard slots emit no binding. Nested patterns recurse over the projected nested product. See [`language.md` § Parameter patterns](language.md#parameter-patterns).

### Surface import-clause additions

Kio surface widens the parenthesized selection list with explicitly tagged
operator grammars and macros. An ordinary name selects the value/type
namespace. Standalone `op` and `varop` selections
are ordinary identifiers; a tag introduces an operator only when followed by
its pattern or delimiter pair. A complete `macro IDENT` selector selects a
macro, with an optional ordinary `as IDENT` destination. `macro` alone or
`macro as local` selects the ordinary identifier named `macro`;
`macro as` selects a macro named `as`. Classification follows from the
complete importing text, without consulting provider contents.

```ebnf
ImportSelective  ::= 'import' ModulePath '(' ImportItem (',' ImportItem)* ')'
ImportItem       ::= NamedImport | OperatorImport | MacroImport
MacroImport      ::= 'macro' IDENT ('as' IDENT)?
OperatorImport   ::= 'op' OpPattern ('as' OpPattern)?
                   | 'varop' VaropHead ('as' VaropHead)?
```

The selection list is nonempty, even when it contains leading or trailing
commas. In canonical formatting, its opening parenthesis stays on the
introducing line immediately after the module path. Its broken layout uses the
ordinary leading-comma rule in
[style.md](style.md#import-block-ordering). An ordinary module import must have
such a list or an explicit `as` alias; builtin block imports have neither.

A fixed `OperatorImport` carries the whole `OpPattern`: every literal run, slot
kind (`_`, `__`, `___`), and later slot. A variadic import carries
its declaration's complete bare or marked square frame. For example:

```kio
import syntax(op _ ? _ : ___, op _ <| ___ |>, varop [* *]);
```

Symbol words are consumed whole. Fixed patterns admit contextual operator-token
quotation where required. Variadic heads use structural brackets and mirrored
marker words; compact and spaced empty heads select the same marker.
The formatter never merges distinct complete grammars into one public spelling.

An optional `as` supplies the complete local grammar, without repeating the
`op` or `varop` tag. A fixed alias preserves every token/slot position, slot
kind, and whole-word boundary. Its token substitution
is consistent and injective: repeated source tokens receive one destination
token, and distinct source tokens stay distinct. A variadic alias supplies an
admitted mirrored OPEN/CLOSE pair and preserves the selected declaration's
folding behavior. Ordinary reserved-syntax and conflict rules apply to the
destination. Neither alias installs the source spelling implicitly.

The written grammar is sufficient to parse and format the consumer without
reading its provider. Semantic resolution subsequently checks the exact named
provider's export and requires equality of the complete grammar. Callable paths,
fold mode, and optional finalizer are implementation details of that selected
export, not part of the import projection. The shorter parser dispatch/conflict
key does not replace this complete import identity (see
[language.md](language.md#operator-grammar)).

Operator and macro selectors are consumed before Kio'; its
`ImportSelective` production admits only `NamedImport`, including its optional
same-class destination.

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
grammar does not decide that predicate. Kio' does not include this alternative.
Compact nominal entries introduce ordinary written type names and add no atom
to the ordinary `Type` grammar. Type positions use exact names such as `Foo`,
`_Foo` and `Foo[T]`; a declaration group belongs only to a `type` declaration.

### Surface expression additions

```ebnf
LiteralCall += (INT_LIT | FLOAT_LIT | STR_LIT+ | BoolLit)                    (* unannotated literal — surface only *)

ExprAtom += TupleLit | BraceUnit | MacroCall | BlockMacroCall | PlaceholderLambda | RecCall

TupleLit          ::= '(' Expr ',' Expr (',' Expr)* ','? ')'                  (* n-ary tuple; n >= 2 *)
BraceUnit         ::= '{' ','* '}'                                            (* unit value *)

MacroHead         ::= IDENT | '(' MacroHead ')'
MacroCall         ::= MacroHead (TypeApplication | ValueApplication)+
BlockMacroCall    ::= MacroHead BlockPrefix? NeutralBlock LabelledBlock* FinalElidedBlock?
BlockPrefix       ::= BlockValueArgs+
BlockValueArgs    ::= '(' (Expr (',' Expr)*)? ','? ')'
LabelledBlock     ::= IDENT NeutralBlock
FinalElidedBlock  ::= IDENT BlockMacroCall
NeutralBlock      ::= '{' ';'* ((NeutralBinding | Expr ';') ';'*)* Expr? '}'
NeutralBinding    ::= LetStmt | PatternLet | ExistentialLet | NeutralBind
NeutralBind       ::= 'let' NeutralBinder '<-' Expr ';'
NeutralBinder     ::= LetBinder | '.' ParamPatternTuple | DotPlaceholderIntro
RecCall           ::= 'recur' RecCallAnnotationList? IDENT (TypeApplication | ValueApplication)+       (* group-local callee *)
RecCallAnnotationList ::= '(' RecCallAnnotation (',' RecCallAnnotation)* ','? ')'
RecCallAnnotation ::= 'poly' | 'cont' | 'escape'
CallArgList       ::= ValueApplication
NonemptyCallArgList ::= '(' Expr (',' Expr)* ','? ')'
                                                                             (* MacroCall's IDENT is any in-scope user-defined macro name —
                                                                                there are no reserved macro keywords; see the prose above *)

DotPlaceholderIntro ::= '.' IDENT '.'                                        (* adjacent tokens; value-binding stem ending in an ASCII letter *)
PlaceholderLambda ::= DotPlaceholderIntro Block                              (* source-local numbered references belong to this stem *)
```

Every block-call head is a single value-shaped identifier through transparent
parentheses; continuation labels are unmarked. The parser records neutral items
without inspecting the provider or its descriptors. `product`, `thunk`,
and `sequence` interpretation and descriptor/content validation happen after
the exact macro declaration is selected. Empty neutral blocks and
leading, repeated, and trailing semicolons are syntactically admitted; the
selected exposure determines their meaning or rejection.

Every nonempty written prefix is a parenthesized value-argument list, including a
single name, literal or expression. `outer(value) { body }` has one prefix,
`outer(a, b) { body }` and `outer((a, b)) { body }` have one product
prefix value. `outer(a)(b) { body }` retains `(a)` as an earlier stage and combines `(b)` with the projected block. Direct `outer() { body }` includes a unit prefix; `outer { body }` has none. Type-shaped argument roots are rejected; type arguments inside ordinary
value expressions such as `outer(f[A]) { body }` remain admissible.

The prefix list fixes ownership of nested expressions before the following
block. `outer(convert(value)) { body }` contains a blockless `convert` call;
`outer(convert(value) { child }) { body }` contains a complete nested block
call. Operator operands obey their ordinary grouping rules within that list.
Only the final labelled block may elide its braces, and then its whole body is
another unspliced block call and its parent is not reverse-spliced. This gives
`if(condition) { yes } else if(other) { maybe } else { no }` its right-nested
ownership without reserving `if`, `do`, `else`, or any library name.

Suffix ownership after an elided child remains with the outer call, but a
reverse splice requires that call's final labelled block to have explicit
braces. Both `outer(a) { A } label inner(b) { B }.<r` and its `.<<r`
counterpart are parse errors, including when parentheses group the whole call.
Write `outer(a) { A } label { inner(b) { B } }.<r` to insert into `outer`,
or `outer(a) { A } label { inner(b) { B }.<r }` to insert into `inner`.
The restriction concerns only the reverse-spliced call's own final labelled
block: a deeper ordinary chain inside an explicit block may still elide braces.
Direct and forward-spliced calls retain ordinary final-label elision.

```ebnf
ExprAtom += VariadicLiteral | ClosedFixedOpExpr

VariadicLiteral  ::= '[' ValueElements? ']'
                   | '[' Marker ValueElements? ReverseMarker ']'
                     (* source-local framing fixes marker and element region first *)
ValueElements    ::= Expr (',' Expr)*                  (* ordinary comma-list rule; value expressions only *)
ClosedFixedOpExpr ::= FixedOperatorUse                 (* selected pattern ends in a fixed word *)
PrefixOpExpr     ::= FixedOperatorUse                  (* selected pattern begins with a fixed word *)
FixedOperatorUse ::= OpUsePart+                        (* exactly one scoped pattern's word/slot sequence *)
OpUsePart        ::= FixedOpToken | Expr               (* each Expr satisfies its selected slot permission *)

Expr += PrefixOpExpr
ExprSuffix += DotSpliceSuffix | ReverseSpliceSuffix | OperatorTail

DotSpliceSuffix   ::= ('.>' | '.>>') DotSpliceCallee NonemptyCallArgList?
                   | ('.>' | '.>>') BlockMacroCall
DotSpliceCallee   ::= GroupedValuePath TypeApplication*                       (* ordinary path or direct local macro name *)
ReverseSpliceSuffix ::= ('.<' | '.<<') TightCallArg
TightCallArg      ::= ExprAtom (TypeApplication | ValueApplication)*          (* stops before another splice *)

OperatorTail     ::= FixedOperatorUse                  (* selected non-prefix pattern continues a preceding Expr *)
```

Placeholder references use the existing single-segment value-path `IDENT` production, not a token or expression alternative. Inside `.stem. { ... }`, the stem followed by a positive decimal index is owned by that lambda unless shadowed by a body-local authored value binder. Indices have no leading zeroes and fit in `u32`. At least one owned reference is required; the highest index determines arity. Explicit lambdas retain the enclosing owner, while nested placeholder lambdas start fresh owners. Qualified paths, type positions, and dedicated recursive callee positions do not participate. Ordinary and macro call heads, including UFCS callees, follow the same lexical ownership. See [Placeholder lambdas](language.md#placeholder-lambdas) for the complete source-local ownership rule.

These schematic fixed-operator productions instantiate the complete pattern explicitly declared or imported in scope; they are constrained by the operand, boundary, maximal-extent, and opposing-claim rules in [`language.md` § Operators](language.md#operators). A final fixed word completes an atom; an operand-ending pattern does not. Their words contain neither dots nor brackets. Variadic framing fixes the marker and comma-list region from source before binding lookup; raw types and forall entries cannot be elements. Both forms lower to ordinary calls before Kio'.

### Surface statement additions

```ebnf
Stmt += PatternLet | ExistentialLet

PatternLet        ::= 'let' ('.' ParamPatternTuple | DotPlaceholderIntro) '=' Expr ';'
ExistentialLet    ::= 'let' '.' '(' ExistsBinder+ ExistentialLetBinder ')' '=' Expr ';'
ExistentialLetBinder ::= IDENT | ParamPatternTuple                           (* existing parameter as-pattern disambiguation applies *)
```

`ExistentialLet` denotes the two-step CPS call against the underlying newtype's CPS projector — see [`language.md` § Existential type binders](language.md#existential-type-binders). Inside a neutral block, parsing retains the binding and its following items without selecting a block exposure. Its value binder is a name or a parenthesized parameter pattern under the existing parameter as-pattern disambiguation. Examples include `(left: Hidden, right: Hidden)` and `(left: _, right: _)`; an untyped opening pattern `(left, right)` is not admitted. Nested patterns and wildcard components retain the parameter-pattern rules. The existential witnesses scope over the pattern's annotations and the rest of the block, with the same escape check as a named opening.

`PatternLet` extends ordinary `let NAME = ...;` with the explicit `let .(PATTERN) = ...;` form. Typed unary `let .(x: T) = e;`, tuple `let .(x, y) = e;`, and as-pattern `let .(whole: (x: A, y: B)) = e;` bindings retain the same checking and projection semantics as parameter patterns. A unary name-only pattern formats as ordinary `let x = e;`. Tuple/as-patterns lower to a scrutinee binding and ordinary projection lets. Existential patterns are pure `=` bindings, including in a neutral block; they are not `<-` binders.

`let .stem. = e;` and a neutral `let .stem. <- e;` introduce a numbered binding with the same adjacent-dot spelling and index rules as a placeholder lambda. The highest owned index in the remaining block determines the unannotated pattern's slot count. The initializer is outside the new scope; later same-stem bindings shadow the family, while different-stem bindings retain it. Actual nested placeholder lambdas remain ownership barriers. See [Numbered bindings](language.md#numbered-bindings).

At statement start, `let` is structural before a name/wildcard binder, before `.` followed by `(`, or before a complete `DotPlaceholderIntro`. The parenthesized route commits to a rich binding pattern; it is not an expression lookahead across the group. Thus `let(f)` remains an ordinary call, including in an expression such as `let(f) <- x` when the operator is declared. Qualified `let.x` references and their operator uses retain their ordinary meaning. There is no keyword-free binding alias. Kio' retains only its ordinary name/wildcard `LetStmt` production.

### Notes (Kio surface)

- The surface `LiteralCall` extension makes the trailing `'[' Type ']'` annotation optional — the unannotated literal is admissible in any expression position. The literal-type selection rule in [`language.md` § Literals](language.md#literals) uses an explicit annotation or concrete contextual inference while checking the Lowered tree. Kio' requires the annotation; the Lowered → Prime substitute pass materializes the typer's recorded resolution, so the Prime artifact is fully annotated.
- A `LiteralAliasValue` accepts exactly one bare literal token. Adjacent string-literal folding applies to expression literals, not to `literal` declaration bodies; write the complete literal in one string token.
- The dot-splice forms are one syntax family. Forward forms take a path-shaped callee with optional explicit type applications and select its immediately following value group. Reverse forms are suffixes on the preceding expression and select its outermost value application. Whole-expression grouping is transparent; a trailing type application is a boundary that makes the receiver a new value stage. Complete block calls instead use first/last prefix insertion. An omitted selected UFCS value group is receiver-only; a present selected group must be nonempty. `r.>f()`, `r.>>f()`, `f().<r` and `f().<<r` reject, including member/macro analogues. An earlier ordinary empty group is unaffected: `factory()(b).<r` modifies `(b)`. The forward callee's adjacent group belongs to that splice, and cannot be reinterpreted as a call on its result. A tight reverse receiver stops before another splice: `f.<x.>g` means `(f.<x).>g`; use `f.<(x.>g)` to insert a splice result.

- A block-call splice inserts its receiver first (`.>` / `.<<`) or last
  (`.>>` / `.<`) among the prefix values, before projection combines the final
  prefix value and whole block values into one right-associated product.
  Reverse splices follow the complete block chain, not its prefix:
  `f(a) { B }.<r` means `f(a, r) { B }`; `f(a).<r { B }` is rejected as
  a same-call arrangement. Grouping is transparent,
  including `((f(a) { B })).<r`.
  One call admits only one splice; grouping does not reset this limit.
  A following forward splice consumes the completed result.
  No further continuation labels attach after a reverse receiver, and a
  reverse splice cannot reopen a block call followed by a result application
  such as `f { B }(x)`. A reverse splice inside the prefix list remains an ordinary
  expression, as in `f(g.<r) { B }`.
  Empty prefix lists are rejected for every block-call splice:
  `f() { B }.<r` rejects; use `f { B }.<r` for no additional prefix value,
  or `f(()) { B }.<r` for an additional Unit. Direct `f() { B }` keeps its
  ordinary Unit meaning.
- Non-block dot-splice value lists contain values only. Brackets on the written callee instantiate its exposed binders before receiver insertion. Forward `.>` / `.>>` select the immediately following value group; reverse `.<` / `.<<` select the immediately preceding outermost value application. Insertion is first for `.>` / `.<<`, last for `.>>` / `.<`. A trailing type application is a boundary: `f(a)[B].<r` applies the instantiated result to `r`; it never searches backward to modify `(a)`. Written stages, not declaration groups or inferred argument types, select the receiver's stage.
- `rec(loop)` items and `recur` calls are surface-only. Groups lower to state-packet `newtype`, ordinary `fn`, closure, and call forms before Kio'; generated packets use an explicit singleton `rec type` or one minimal bare `rec { ... }` type group when their payload graph is cyclic. Compact `rec type` likewise preserves genuine recursive components through those ordinary Kio' declarations. The capability-free data forms are not recursive-function syntax. A recursive function call uses `recur`; `poly` marks a type-changing call, `cont` marks a non-tail call, and `escape` marks a call captured across an ordinary closure boundary. `escape` entails `cont`; both may be written, while canonical formatting omits the entailed `cont`. Each annotation must satisfy the contextual rules in [`language.md` § Recursive functions](language.md#recursive-functions).
- An expression's fixed-operator scope contains exactly its written imports and local declarations preceding the enclosing item, including lazily parsed bodies. A leading word without a registered prefix pattern is rejected. Native atom introducers, whole pending following words, and square-frame edges have their source-local roles before custom dispatch. The signed-number rule includes expression-ending `]`, so `[a, b]-1` has subtraction shape and `[-1]` remains a signed-literal frame. Missing variadic bindings never change a frame's marker.
- A program that parses against the Kio' productions also parses against the full Kio grammar with the same meaning. It needs none of the additional surface productions; the full grammar is a strict superset.
- **Native structural prefixes.** Required native symbols such as `->`, existential `>`, kind stars, and type-chain separators retain their grammatical prefix handling. Brackets and dots are already structural, preserving `[*F]`, `[A][*F]`, and `.[*F]`. Native dot compounds retain their adjacent spelling. User-operator positions consume complete words and never carve a word into shorter imported operators.
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
- `BridgeBlock` is a `;`-separated list of module-path globs spelled the same way a `ModulePath` is, extended with `*` (matches one path segment) and `**` (matches a whole subtree, zero or more segments) following shell glob semantics. It selects **modules**; from each matched module, every `pub host` item becomes a host requirement (the env — what the host must supply), every other public callable becomes a host-invocable entry (an export), and public type declarations contribute the type surface those signatures use. A plain-public nominal type contributes its identity; its constructor and projector contribute independent host operations only when the corresponding member is also plain `pub`. A private or `pub(path)` outer nominal type contributes no host surface regardless of its member markers. Entries keep their module namespace; there is no rename. A glob that matches no module is a build error. The module-completeness and type-closure rules that keep the exposed contract self-contained are stated in [`package.md` § The bridge block](package.md#the-bridge-block).
- `pub` is not admissible on package-file entries. Only the optional `build` and `bridge` blocks follow the required package header.

### Build block

The optional `build { ... }` block inside a package file carries the package's compilation-target configuration. It may precede or follow the optional `bridge` block after the required package header. There is no separate build-file shape.

```ebnf
BuildBlock        ::= 'build' '{' SemiRunEntries<BuildEntry> '}' ';'?
BuildEntry        ::= CacheDecl | DocsDecl | TargetBlock
CacheDecl         ::= 'cache' BlockFieldValue
DocsDecl          ::= 'docs' '{' SemiRunEntries<DocsKey> '}'
DocsKey           ::= ('md' | 'support' | 'md_out' | 'html') BlockFieldValue
TargetBlock       ::= 'target' TargetHeader '{' SemiRunEntries<TargetKey> '}'
TargetHeader      ::= BackendId | '(' BackendId ',' ConfigString ')'
BackendId         ::= IDENT ('-' IDENT)*                                       (* a bare kebab-case backend id, e.g. rust, js, kio-prime *)
TargetKey         ::= IDENT BlockFieldValue
```

Notes:

- The leading tokens `build`, `cache`, `docs`, and `target` are contextual keywords inside the build block; elsewhere in a package file they are ordinary identifiers unless another production gives them meaning.
- The `CacheDecl` is **optional** and may appear at most once, anywhere among the build entries. Omission and `cache ()` both disable caching; formatting preserves either spelling (see [`style.md`](style.md)). A `ConfigString` cache value names a filesystem path (relative to the package root, or absolute). Any other value kind is a parse error.
- The `DocsDecl` is **optional**, declared at most once, anywhere among the build entries. Its keys are unordered: `md` is required; `support` is repeatable; `md_out` and `html` are optional and may each appear at most once. Every `DocsKey` accepts a `ConfigString` filesystem path; the optional fields also accept `ConfigUnit`, selecting the same output path as omission. See [`package.md` § Build target files](package.md#build-target-files).
- A `BackendId` is a **bare identifier** naming a backend (`rust`, `js`, `kio-prime`, …). The hyphen-folding lets a kebab-cased backend id like `kio-prime` spell as a single id; each `-` and the following `IDENT` must be flush. In a bare header, the backend spelling is also the target name. A paired header supplies a distinct string name: `target (js, "web")`. The pair is backend first and is not an expression. Target names must be unique within the block, including collisions between bare and paired headers; duplicates are a build error (exit code `40`). Different names may select the same backend. Unknown backends are build errors, not parse errors.
- Host-target `namespace` accepts `ConfigString` or `ConfigUnit`; the latter selects the same package-derived namespace as omission. Host-target `baseline` accepts a backend's concrete named choices, `latest`, or `ConfigUnit`, as specified in [package.md § Per-target keys](package.md#per-target-keys). Rust's `thread_safety` accepts the bare choices `send`, `sync`, `send_sync`, or `ConfigUnit`, with transparent grouping around a choice. Strings are rejected for `baseline` and `thread_safety`. All other target-key values are `ConfigString`. Unknown keys and unknown well-formed named choices are build errors; an unrecognized malformed word is a parse error.

The build block's field-value grammar:

```ebnf
BlockFieldValue   ::= '(' ')' | STR_LIT | NUM_LIT | IDENT | '(' BlockFieldValue ')'
ConfigString      ::= STR_LIT | '(' ConfigString ')'
ConfigUnit        ::= '(' ')' | '(' ConfigUnit ')'
NUM_LIT           ::= INT_LIT | FLOAT_LIT
```

A field value is the unit literal `()`, a string literal, a number literal, or a bare word — booleans arrive as the bare words `true` / `false`, which lex as `IDENT`. Each field validates which of these kinds it accepts. A field's recognized closed choices, such as `baseline v5_0` for TypeScript, are configuration words rather than ordinary value names; other bare words follow the value-name grammar. This does not admit those configuration spellings as declarations or references.
Repeated grouping is transparent and does not expand a field's accepted value
kinds. Commas, calls, and operators are not part of this scalar grammar.

### Dependency file (`*.dep.kio`)

A dependency-declaration file names one direct cross-package dependency. It begins with a `dependency <local>;` header, where `<local>` is the filename stem (the same filename-stem coherence check `package <name>;` and `signature <name>;` run), and contains exactly one `source { … }` block naming where the dependency comes from — a local package file (`path`) or a remote git repository (`git` + `ref`).

```ebnf
DependencyFile    ::= DependencyDecl DependencyEntry*
DependencyEntry   ::= SourceBlock | RehostStmt | RetypeStmt
DependencyDecl    ::= 'dependency' IDENT ';'                    (* IDENT = filename stem = dependency local name; lowercase value name *)

SourceBlock       ::= 'source' '{' SemiRunEntries<SourceField> '}' ';'?
SourceField       ::= ('path' | 'git' | 'ref') ConfigString | 'path' ConfigUnit
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
- A Git source's `path ()` is equivalent to omitting `path`; a local source requires a string path. Grouping is admitted around each string or unit value. The formatter preserves omission and explicit `path ()` without performing discovery.
- A **`rehost`** statement is `rehost <from> to <to>;`, where `<from>` and `<to>` are each a `/`-separated module path with no trailing item. `<from>` names a re-rooted dependency module (its leading segment is the dependency's local name); `<to>` names a consumer module that provides replacements for `<from>`'s host items. A dependency file may carry any number of `rehost` statements in any order relative to `source` and `retype`; their source order is not significant (the formatter sorts them). The rebinding semantics — rewriting `<from>`'s `host` items in place into forwarding aliases / wrappers onto `<to>` during materialization — are in [`package.md` § Dependency files](package.md#dependency-files).
- A **`retype`** statement is `retype <from> to <to>;`, where `<from>` and `<to>` are each a `/`-separated module path optionally followed by a trailing `.<Name>` selecting one `newtype` (a **TYPE** name). The trailing `.<Name>`, when present, must appear on **both** sides with the **same** name (the per-type form); omitting it on both sides remaps every `newtype` under the module. `<from>` names a re-rooted dependency module (leading segment = the dependency's local name); `<to>` names the counterpart module — another dependency's module or one of the consumer's own — holding the same-named `newtype`s. A dependency file may carry any number of `retype` statements in any order relative to `source` and `rehost`; their source order is not significant (the formatter sorts them, after the `rehost` group). Those statements must select pairwise-disjoint exact source newtypes: after expanding a module-form statement over the newtypes its source module actually declares, no exact `(module, newtype name)` may be selected again by another module-form or per-type statement, even when the second statement names the same target. The remap semantics — removing `<from>`'s matched `newtype`s and importing the same names from `<to>` during materialization, gated on a same-named, structurally-congruent counterpart — are in [`package.md` § Dependency files](package.md#dependency-files).

### Dependency lock file (`*.lock.kio`)

A dependency lock file pins the exact commit a remote `git` dependency's `ref` resolved to, plus the dependency's contract-surface digest at that commit. It is written beside the `<local>.dep.kio` it locks (same stem), is committed to version control, and begins with a `lock <local>;` header (the same filename-stem coherence check), followed by exactly one `resolved { … }` block.

```ebnf
LockFile          ::= LockDecl ResolvedBlock
LockDecl          ::= 'lock' IDENT ';'                          (* IDENT = filename stem = dependency local name; lowercase value name *)

ResolvedBlock     ::= 'resolved' '{' SemiRunEntries<ResolvedKey> '}' ';'?
ResolvedKey       ::= 'git'    ConfigString                     (* the clone URL the lock pins *)
                    | 'ref'    ConfigString                     (* the revision designator requested *)
                    | 'path'   (ConfigString | ConfigUnit)      (* optional repository-relative package manifest *)
                    | 'commit' ConfigString                     (* the commit SHA the ref resolved to *)
                    | 'sig'    ConfigString                     (* the dependency's contract-surface digest at the commit *)
```

Notes:

- The file must begin with a `lock <local>;` directive whose `<local>` is the filename stem (for example, `foobar.lock.kio` begins `lock foobar;`). A missing directive or a name that does not match the stem is a parse error. `lock` is a file-shape keyword, a sibling of `dependency`; it is recognized only in this leading position.
- The `resolved` block declares the required `git`, `ref`, `commit`, and `sig` keys and an optional `path` in any order. Values are grouped or ungrouped strings, with `path ()` equivalent to omitting `path`. A duplicate key, a missing required key, another value kind, or any other key is a parse error. A string `path` has the same relative-syntax rules as a Git source's selector. Anything after the `resolved` block is a parse error.
- The `sig` value is the dependency's contract-surface digest at the pinned commit (see [`versioning.md` § Git-dependency contract gate](versioning.md#git-dependency-contract-gate)); it is opaque to the grammar — any `STR_LIT`.

### Signature file (`*.sig.kio`)

A signature file records a package's versioned **contract-surface changelog** — the history of its host / export surface across sealed generations. At most one per package; stem = package name. It begins with a `signature <pkg> v(N);` header (the same filename-stem coherence check the other package-root shapes run), is generated and gated by `kio sig`, and — like the dependency files — is not a module and is kept out of module discovery.

```ebnf
SignatureFile     ::= SignatureDecl VersionBlock*
SignatureDecl     ::= 'signature' IDENT 'v' '(' INT_LIT ')' ';'   (* IDENT = filename stem; v(N) = current generation *)
```

The version-block body admits optional `with`, `breaking`, and `nonbreaking` entries in any order, at most once each. Each partition admits `add`, `modify`, and `remove` operation blocks in any order, at most once each. Ordinary nonrecursive add/modify entries remain post-elaboration Kio' declarations inside `module` sections. A complete recursive `rec { ... }` group instead appears exactly once under `with { module <path> { ... } }`, while operations name its changed members by exact non-expression `ItemRef` FQNs such as `api.A;` or `foo/bar.B;`. Canonical removals use the same FQN form; the legacy nested `module path { Name; }` removal form remains parse-compatible but is not emitted. The complete grammar and context-epoch/replay rules are specified in [`versioning.md` § Changelog grammar](versioning.md#changelog-grammar).

The one changelog-specific declaration production is the **body-less `fn` export signature**, which uses the ordinary function's `Signature`, including type-only groups, and is recorded without a body (a stray body is a changelog error). Unlike a host import, an ordinary export does not require a value group:

```ebnf
SigExportFn       ::= ('pub' 'pure'? | 'pure' 'pub') 'fn' IDENT Signature ('->' Type)?
                                                                  (* changelog-only; body-less export; formatter emits `pub pure fn` *)
```

`signature` is a file-shape keyword, a sibling of `package` / `module` / `dependency` / `lock`, recognized only in this leading position.

### Notes (Package files)

- Package files share `ModulePath`, `Signature`, `Type`, `Block`,
  `TypeParamList`, and the literal forms with the regular-module grammars;
  productions are not duplicated above.
- A package file admits no `module ...;` directive and no `import` clauses (see § Package file Notes above).
- `kio fmt` formats package files with the same A1 leading-comma rules as regular modules; see [`style.md`](style.md). Each `bridge` glob is formatted on its own line.

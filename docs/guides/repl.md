# Exploring modules with `kio repl`

`kio repl` opens an interactive prompt for *inspecting* the Kio modules in a directory. You load modules by their module path, ask for the type or purity of a value, read its documentation, print its canonical source, and follow cross-references between modules. It is the fastest way to answer "what is this, and where is it used?" without leaving the terminal.

This guide covers the inspector surface: the meta-commands, the synonym pairs, tab completion, auto-reload, and the slash module-path model. It works on any directory containing `.kio` modules; a `*.pkg.kio` package file is optional.

## What `kio repl` is — and is not

`kio repl` is run from a directory containing `.kio` modules:

```text
$ cd my-package
$ kio repl
kio repl — the module inspector
3 modules available — `:load <module>` to bring one in (`:mods` lists what's loaded)
type `:help` for commands, `:quit` to leave
kio>
```

No package file is required: a directory of loose `.kio` modules works, and an empty directory opens on a blank slate (the banner reads `(no modules in this directory)`). The REPL never loads anything at startup unless you pass a `<selector>` — you bring modules in explicitly with `:load`.

The prompt is two-line: above the `kio>` input symbol, the REPL renders the current module's path — or `(no module — :load <module-path> to begin)` before anything is loaded — so a bare-name query always shows which scope it resolves against. The transcripts in this guide elide that context line to stay compact.

It is a module inspector, not a host runtime. Loading or refreshing a module checks the directory snapshot through parsing, operator folding and surface lowering, name resolution, and Lowered typechecking. Expression queries additionally substitute the recorded completions and validate the resulting Prime artifact; `:normalize` reduces the checked expression through the compiler's host-independent reduction relation. The REPL therefore shows the same types and signatures as the compiler without executing the package against a host. `kio repl` is a typed lens onto a directory's modules, not a place to run programs.

Two consequences follow:

- The prompt accepts **meta-commands and queries, but no definitions**. You cannot type a `fn` or an `import` clause; what you can type is a `:`-prefixed meta-command or a name or Kio expression to inspect. An expression is type-checked and, for `:normalize`, reduced; it is never run against a host.
- Every answer is **compiler-accurate**. `:t` shows a function's synthesized type; `:pure` applies the compiler's pure-function rule; `:signature` shows its declaration header; `:doc` renders the same documentation `kio doc build` would; `:source` prints exactly what `kio fmt` would; an expression typed on its own prints its type and the residual form the compiler uses for `equiv` checks.

## A first session

Suppose the package — call it `my_package` — has a module `my_package/examples`. Its body declares two documented placeholder functions (the snippet below is that module body):

<!--kio {harness=module placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type Int role(i32);
host fn add(p0: Int, p1: Int) -> Int;
host fn mul(p0: Int, p1: Int) -> Int;

__INSERT_CODE_HERE__
-->

```kio {@module}
/// An example function that multiplies its two arguments.
///
/// See [`example_fn2`] for a related example.
pub fn example_fn1(x: Int, y: Int) -> Int { mul(x, y) }

/// Another example function that multiplies its two arguments.
pub fn example_fn2(x: Int, y: Int) -> Int { mul(x, y) }
```

Load the module and ask about `example_fn1`:

```text
kio> :load my_package/examples
loaded my_package/examples
kio> :t example_fn1
example_fn1 : (Int & Int) -> Int
kio> :signature example_fn1
pub fn example_fn1(x: Int, y: Int) -> Int
kio> :doc example_fn1
An example function that multiplies its two arguments.

See `example_fn2` for a related example.

pub fn example_fn1(x: Int, y: Int) -> Int
kio> :source example_fn1
pub fn example_fn1(x: Int, y: Int) -> Int { mul(x, y) }
```

`:t` gives the type, `:signature` the declaration header, `:doc` renders the doc-comment followed by the signature, and `:source` prints the canonical declaration with its body.

## The meta-command set

A `<name>` argument is resolved through the **current scope**: the items the current module declares, the names it imports, and any module alias it sets. `:scope` enumerates exactly this set. A fully-qualified item path — `my_package/examples.example_fn1` (module `my_package/examples`, item `example_fn1`) — resolves directly, regardless of which module is current. A `<module-path>` argument is a loaded module's path.

| Command | What it does |
|---------|--------------|
| `:load <module-path>` | Load a module (and its `import`-clause dependencies) |
| `:signature <name>` | Print a name's declaration header |
| `:source <name>` | Print a name's canonical source (header and body) |
| `:t <name-or-expr>` | Print the synthesized type of a name or an expression |
| `:pure <name-or-expr>` | Report whether a function or expression satisfies the compiler's purity rule |
| `:normalize <expr>` | Show what the expression reduces to — the form the compiler uses when checking `equiv` proofs |
| `:doc <name>` | Render a name's doc-comment and signature |
| `:ls [-v] <module-path>` | List a loaded module's items (`-v` for full signatures) |
| `:mods` | List loaded modules — `*` marks the current one |
| `:packages [<package>]` | List the `*.pkg.kio` packages, or view one's contract |
| `:scope [-v]` | List everything in the current scope (`-v` for full signatures) |
| `:unload <module-path>` | Remove a loaded module |
| `:which <name>` | Which loaded module declares a name |
| `:refs <name>` | Every reference to a name across loaded modules |
| `:help` | List the commands |
| `:reset` | Drop every loaded module |
| `:quit` | Leave the REPL (Ctrl-D also works) |

`:signature`, `:source`, and `:t` are the **named-item query trio** — three fixed views of one named item: its declaration header, its full source, and the type of the value it binds. Kiodoc spells the same three as the `@signature` / `@source` / `@type` directives; the REPL and Kiodoc share one renderer per view, so a `:signature` query and the matching `@signature` directive produce identical output.

`:which`, `:refs`, and `:scope` are the navigation commands. `:which example_fn1` reports the fully-qualified `my_package/examples.example_fn1` (private and public declarations both count); `:refs example_fn1` lists every call site, type-position use, and `import`-clause mention of `example_fn1`, each as `module-path:line:col  in <kind> <name>` with the enclosing top-level item's spelling; `:scope` lists everything in the current module's scope under five sections (declared items, imported names by source, operator bindings, module aliases, intrinsics state).

`:packages` (synonym `:pkgs`) is the package counterpart to `:mods`. Packages are optional — the REPL works on the module tree whether or not a `*.pkg.kio` is present — so this is a read-only view that never touches the loaded-module set. With no argument it lists every `*.pkg.kio` in the directory tree; with a package name (the `.pkg.kio` stem) it shows that package's contract: the package name and its `bridge { … }` block — the module globs that select which modules' `pub` surface (host requirements plus exports) the package exposes.

### Synonyms

Most commands have a short and a long spelling. Both are first-class — use whichever reads better (and `:help` answers to `:?` as well as `:h`):

| Short | Long | Short | Long |
|-------|------|-------|------|
| `:l` | `:load` | `:sig` | `:signature` |
| `:u` | `:unload` | `:src` | `:source` |
| `:q` | `:quit` | `:t` | `:type` |
| `:h` / `:?` | `:help` | `:norm` | `:normalize` |
| `:ls` | `:list` | `:mods` | `:modules` |
| `:refs` | `:references` | `:pkgs` | `:packages` |

## Expression queries

A line that doesn't start with `:` is a bare query. The REPL looks at what the line is and dispatches it the same way the matching meta-command would:

- A **name** — a single identifier, a module-qualified path, an operator symbol — routes to `:doc`: it prints the name's doc-comment and signature (or, for a loaded module path, the module summary).
- Any **other expression** — a literal, an application, an operator expression — prints its type first, then what the expression reduces to: the `:type` and `:normalize` views together. Expressions that call host functions usually can't reduce all the way, so the type line is the dependable summary even when the reduced form stays partial.

Either way, a dim footer lists the other commands that fit the same input — each under its full spelling, in the order `:help` lists them — so you can see your options. Input that is neither a name nor a Kio expression prints one line pointing you at `:help`.

```text
kio> example_fn1
An example function that multiplies its two arguments.

See `example_fn2` for a related example.

pub fn example_fn1(x: Int, y: Int) -> Int
(also: :signature  :source  :type  :pure  :normalize  :doc  :which  :references)
kio> example_fn1(3, 4)
example_fn1(3, 4) : Int
mul(3(Int), 4(Int))
(also: :type  :pure  :normalize)
```

A single identifier on its own is read as that name, not as a zero-argument call. To reduce a single name instead of viewing its doc, use `:normalize` explicitly: `:normalize example_fn1` reduces it; `example_fn1` on its own prints the doc-comment and signature.

`:normalize <expr>` shows what the expression reduces to: the REPL simplifies as far as it can — running through function bodies, substituting `let`-bindings, picking `match!` branches — and stops at `host` items it can't see into and free variables it has nothing to substitute for. Host calls remain visible even when their `()` result is discarded, so an extra `print(...)` still changes the residual form. A function result is summarized by where it came from and which parameter groups remain, for example `<closure provider.wrap(x)(y)>`. This is an explanation, not Kio code: the REPL does not repeat the function body or its captured values, and two matching closure summaries are not proof that the functions are equivalent. If reduction leaves a newtype constructor or projector from a dependency in any other result that the current module cannot identify clearly, the REPL prints ordinary qualified `import` lines that distinguish the declarations, then a blank line and the reduced expression. Those lines explain which declarations the answer means; they do not change the loaded module's scope or promise that copying the output produces a complete program. Private declarations and missing surrounding type information can still keep such a copy from type-checking. The REPL omits the extra lines when the current module already has an unshadowed name for every declaration. Reduction uses the same rules as `equiv` proofs (Kio's mechanism for asserting two expressions are equivalent; see [`specs/formal/equiv.md`](../../specs/formal/equiv.md)); the informational closure summary is only a display of that internal result.

`:t` reports just the synthesized type — the same line a bare expression already prints first — and accepts either a name/FQN or an expression. These remain distinct input categories: a spelling that resolves as a declared name or exact item FQN, such as `my_package/examples.example_fn1`, or names a loaded module uses the name branch. Otherwise, a path-shaped spelling that parses in the current module uses the expression branch. That includes an alias-qualified value path such as `value.item`, and a slash-operator expression with either a bare or dotted right operand when `/` is imported. Once that expression parses, any type error is reported as an expression error; if the expression parse also fails, the name-resolution error is reported. Asked of a type-level name (a `newtype`, `type`, or `labels`), `:t` reports a kind error: a type-level name binds no value, so it has no type to print (use `:signature` for its declaration header). Asked of a `literal` alias, `:t` asks for an annotated expression such as `name(Type)`, because the literal gets its concrete type from the use site.

`:pure` accepts the same two categories and prints either `pure` or `impure`. For a declaration name or exact FQN, it reads the declaration contract: a `pure fn` is pure, while an unmarked `fn` or a `host fn` is impure. Other declaration kinds receive a kind-aware error because they are not executable value bindings. For an expression, the REPL asks whether the compiler would admit that expression as the body of an ordinary `pure fn`. If only an unrestricted function admits it, the answer is `impure`; if neither context admits it, the REPL shows the underlying syntax or type error instead. This is a compile-time query and does not evaluate the expression. The pure context includes nested lambda bodies, so a lambda that refers to a host function is impure even when the query only constructs the lambda and never calls it.

```text
kio> :pure example_fn1
impure
kio> :pure .(x: .) { x }
pure
```

```text
kio> :t example_fn1(3, 4)
example_fn1(3, 4) : Int
kio> :normalize example_fn1(3, 4)
mul(3(Int), 4(Int))
```

An expression that does not parse, or does not type-check, reports the error and leaves the session unchanged. Expression queries resolve names through the current scope, so load a module first; with nothing loaded, queries with short names have nothing to resolve against.

`:t` and `:pure` accept both categories described above. `:normalize` accepts only an expression; `:load`, `:unload`, `:ls`, `:signature`, `:source`, `:doc`, `:which`, and `:refs` accept a name or fully-qualified item path. A slash-qualified item path is not a Kio expression: source code must import the item selectively or import its module under an alias and use `alias.item`. The REPL can still answer `:pure my_package/examples.example_fn1` without an import because that input uses `:pure`'s separate FQN/name branch. When a single-category command fails and the argument looks like the *other* category — a slash-qualified FQN handed to `:normalize`, or an expression like `1 + 2` handed to `:signature` — the error adds a one-line nudge toward the command that fits. The nudge is only a suggestion: a command that succeeds is never second-guessed, and the REPL never quietly switches commands on you.

## The module-path model

Kio modules are named by a module path — `my_package/examples`, `my_package/util/text` — and `:load` takes that path, resolving it the same way an `import` clause does. Loading a module makes it the **current** module: a name typed on its own in subsequent commands resolves through its view.

`:load` also pulls in dependencies. When a module's `import` clauses name other modules of the same package, those are loaded too — *implicitly*. `:mods` distinguishes the two:

```text
kio> :load my_package/app
loaded my_package/app
  pulled in: my_package/examples
kio> :mods
* my_package/app
  my_package/examples (via my_package/app)
```

The `*` marks the current module. `my_package/examples (via my_package/app)` is an implicit module — it was not named in a `:load`; it came along because `my_package/app` depends on it.

The whole load is atomic. If the package does not typecheck, or a dependency cannot be found, `:load` reports the error and changes nothing — the session keeps whatever was loaded before.

`:unload` respects the same structure. Unloading an implicit module is rejected — you unload its explicit referent instead, and any implicit dependency left with nothing depending on it is removed automatically. Loading the same path again switches the current module back to it; this is the natural gesture for moving focus between modules.

## Tab completion

Press Tab to complete. At the start of a line, completion offers `:command` spellings — both the short and long forms. In an argument position, it offers the names the session knows: package module paths, the item names of loaded modules, and their fully-qualified paths.

Expression completion uses the current module's declarations and explicit imports, together with locals bound in the expression being typed. Loading another module does not make its names available as bare expression names. An inner binding hides an outer binding with the same name. Commands that inspect declared names, such as `:source` and `:doc`, still browse the loaded modules' names and fully-qualified paths.

Expression suggestions also follow the syntax at the cursor: type positions
offer type names, operator positions offer the selected operators, and closed
keyword positions offer the choices valid there. Suggestions respect type/value
naming namespaces and insert canonical spellings. Comments and string contents do not receive unrelated
name suggestions.

The menu displays a bounded portion of the results while retaining all matches.
In the terminal, press Tab to move through the candidates, Enter to accept one,
or Escape to dismiss the menu. A match beyond the first displayed portion is
still reachable.

```text
kio> :lo<Tab>
kio> :load
kio> :load my_package/<Tab>
my_package/app   my_package/examples   my_package/util
```

## Auto-reload

While a REPL session is open, editing any `.kio` file of the package on disk triggers a re-typecheck. The check runs the next time you submit a command, so that command sees the current file contents:

```text
kio> :t example_fn1
example_fn1 : (Int & Int) -> Int
   ... you edit examples.kio in your editor, adding an `example_fn3` function ...
kio> :ls my_package/examples
reloaded my_package/examples
my_package/examples:
  pub fn example_fn1
  pub fn example_fn2
  pub fn example_fn3
```

The `reloaded` line tells you the session picked up the change. If your edit introduced a type error, a diagnostic prints instead and the session keeps the last version that checked — a broken edit never silently discards a working session. Auto-reload is always on.

## Highlighting

When `kio repl`'s output goes to an interactive terminal, types, signatures, and source spans are syntax-highlighted. When the output is redirected to a file or a pipe, or the terminal is `dumb`, the REPL falls back to plain text — so capturing a session transcript gives you clean, escape-free text. The `NO_COLOR` convention is honored.

## Where to go next

`kio repl` is for *exploring* a package. To *build* one, see [the tooling tutorial](../tutorials/tooling.md); for the language itself, [the language tutorial](../tutorials/language.md). The full `kio repl` contract — every command, every exit code — is in [`specs/cli.md`](../../specs/cli.md).

# Roadmap

Major in-flight design threads, in rough priority order. Entries sketch where Kio is heading; specifics land in `specs/` once settled.

## Host-language expansion

C, C++, Zig, WASM, C#, F#, Kotlin, Scala, OCaml, PureScript, Ruby, PHP, Lua, Dart, Julia, Clojure, Racket, Scheme, Common Lisp, Erlang / Elixir.

## Dynamic loading via host JS / WASM engines

Kio already loads Kio' programs at runtime, via an interpreter written in Kio; the host calls into them through a statically typed interface, so a type mismatch is caught when the calling code is compiled, not as a runtime crash. Running a loaded program on the host's own JS or WASM engine extends this to native execution.

## REPL authoring

`kio repl` already evaluates expressions against a loaded package. REPL authoring extends this to incrementally enter, revise, and retract definitions in the session, with the session staying a valid module throughout.

## Escaping recursive calls

Adding a `rec(escape)` annotation for recursive calls captured by closures that can re-enter the same recursive group after the current loop step has returned, while preserving Kio' as a non-recursive core.

## Elaborator quoting

Giving elaborators a quoting facility for producing generated Kio glue from ordinary term syntax, reducing the amount of hand-built term construction in elaborator code.

## Self-hosting

Rewriting the Kio compiler in Kio.

## Auto-generated catamorphism

Compiler-generated structural folds over recursive `newtype`s — letting user code traverse data structures by describing the fold rather than the recursion.

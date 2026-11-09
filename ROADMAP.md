# Roadmap

Major in-flight design threads, in rough priority order. Entries sketch where Kio is heading; specifics land in `specs/` once settled.

## Host-language expansion

C, C++, Zig, WASM, C#, F#, Kotlin, Scala, OCaml, PureScript, Ruby, PHP, Lua, Dart, Julia, Clojure, Racket, Scheme, Common Lisp, Erlang / Elixir.

## Dynamic loading via host JS / WASM engines

Kio already loads Kio' programs at runtime, via an interpreter written in Kio; the host calls into them through a statically typed interface, so a type mismatch is caught when the calling code is compiled, not as a runtime crash. Running a loaded program on the host's own JS or WASM engine extends this to native execution.

## Host debugging aids

Optional runtime type checks and detection of shared-data mutation that violates
the host contract, for dynamic-language hosts.

## REPL authoring

`kio repl` already evaluates expressions against a loaded package. REPL authoring extends this to incrementally enter, revise, and retract definitions in the session, with the session staying a valid module throughout.

## Macro literal inspection

Letting macros inspect natural-number and string literals to support tuple indexing, efficient pattern matching against string literals, and compile-time regular-expression compilation.

## Macro quoting

Giving macros a quoting facility for producing generated Kio glue from ordinary term syntax, reducing the amount of hand-built term construction in macro code.

## Self-hosting

Rewriting the Kio compiler in Kio.

## Auto-generated catamorphism

Compiler-generated structural folds over recursive nominal types — letting user code traverse data structures by describing the fold rather than the recursion.

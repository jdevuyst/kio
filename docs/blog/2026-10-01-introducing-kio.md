# Introducing Kio

_By [Jonas De Vuyst](https://github.com/jdevuyst)_

Today I'm making version 0.1 of [the Kio Programming Language](https://jdevuyst.github.io/kio/) public.

In some ways the 0.1 version number is underselling the maturity of the language. Kio does have a good number of features already:

- A type system based on polymorphic lambda calculus, with rank-N types, product/row types, sum types, and higher-kinded types.
- Provable equivalences allow the user to assert that two expressions are equivalent. The compiler normalizes the expressions, up to the point where they call host functionality, and verifies that they match.
- Type-driven macros, called elaborators, take a number of type and value arguments and construct a return value. Elaborators see value arguments as opaque boxes, but they can inspect the type arguments and use those to guide the construction of return values. If the result type has a 'waiting for inference' status, the elaborator can additionally 'fill' that type slot. This approach is powerful enough to implement `match!` (for pattern matching) and `derive!` (for deriving the Kio counterpart of trait instances) as user code.
- Tooling includes a REPL, a formatter, an API documentation generator, an LSP server, a Tree-sitter grammar, and a VS Code plugin.
- 8 host languages are supported right now; more are on the way.

Yet there is one important caveat that will raise eyebrows: _Kio was created with AI_.

In the coming weeks I will be blogging about various aspects of Kio. Today I will explain why I think designing a programming language with AI isn't completely unreasonable.

First, there is of course the sheer speed with which I was able to iterate. Conventional wisdom has it that it takes [a two-year sabbatical and a hammock](https://clojure.org/about/history) to create a programming language. The company I work for only allows three-month sabbaticals and Kio was created during such a sabbatical.

Let's consider a 'worst'-case scenario for this project. Suppose that in the near future I come to learn that a compiler really does need to be written by hand.

Even in a scenario where I end up rewriting the entire compiler by hand, I would still benefit from having started the project the way I did. There are several aspects of the language where I didn't get the design right on the first try and where I had to iterate, iterate, iterate. I burned a lot of tokens on some of these design pivots, but if I had had to make those changes by hand, I think I would have needed a three-month sabbatical for these changes alone (and I'm not allowed to take another one). The prospect of writing a compiler by hand sounds a lot less daunting if the language design is settled ahead of time!

I would also benefit from having an extensive test corpus to get me started. (I do know the test corpus has exposed bugs, so it's not completely useless.) The tooling around the compiler also seems to work fine. I'd benefit from not needing to write that by hand.

And that's a worst-case scenario. I have taken a number of steps to steer away from that outcome:

- Agents are instructed to proactively produce [golden tests](https://github.com/jdevuyst/kio/tree/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/test-data/goldens) when adding features and to produce more goldens whenever a bug is discovered.
- I asked AI to create several [POC packages](https://github.com/jdevuyst/kio/tree/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/test-data/poc), such as optics, functional data structures, elaborators, and an interpreter for the core language. The Kio-in-Kio interpreter is itself tested against the same golden tests that are used to test the compiler. The elaborators are used by a good chunk of the golden tests.
- I asked AI to come up with concepts for ['realistic' programs](https://github.com/jdevuyst/kio/tree/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/test-data/castles) and then implement them.
- The agents also maintain a [program generator](https://github.com/jdevuyst/kio/tree/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/ci/infra/kio-gen-rs) that is used for testing. `cargo-llvm-cov` helps ensure that the program generator is sufficiently complete.
- Core functionality is also tested by [fuzzing](https://github.com/jdevuyst/kio/blob/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/reports/fuzz.sh) and [mutation testing](https://github.com/jdevuyst/kio/blob/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/reports/mutation.sh).
- Kio is split up into a surface language and a [smaller core](https://github.com/jdevuyst/kio/blob/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/specs/prime.md). All surface language constructs can be expressed in terms of the smaller core.
- After type checking of the core language completes, a shared intermediate representation is generated that sets the scene for emitting code in the target language. Different backends also share code between several [language families](https://github.com/jdevuyst/kio/blob/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/specs/backends/README.md).
- Agents are instructed to maintain [formal specifications](https://github.com/jdevuyst/kio/blob/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/specs/language.md) and user-facing documentation.
- I regularly ask AI to do an audit of the codebase, and there are [comprehensive instructions](https://github.com/jdevuyst/kio/blob/90bf45cfcc420e61722104c4dcaf85e4c00b4a67/ai/skills/audit/SKILL.md) that enumerate what is expected of such an audit. (Fun fact: this blog, too, is included in the audit as a source of truth.)

I mentioned that the compiler will lower the surface language to a core language. Let me explain this in more detail:

1. A [Trees That Grow](https://www.cs.tufts.edu/comp/150FP/archive/simon-peyton-jones/trees-that-grow.pdf) approach makes it so that surface language constructs are unrepresentable at later phases of the compiler.
2. There's a CI pass where the compiler will emit core-language constructs only. This is then tested against a separate program that verifies that the compiler output indeed does not have surface language constructs. Next, the core code is compiled to other targets with a subset of the Kio compiler that can only handle the core language (via conditional compilation), and this output is compared against the full compiler that directly compiled the original source code.
3. The audit skill guards the integrity of this approach.

If I have tokens to burn at some point, I may also want to experiment with having AI generate formal correctness proofs. This could consist of mathematical proofs—verified by a proof assistant—that the specifications are sound and complete, a parallel implementation in Agda/Coq/Lean, and/or formal verification of the Rust code. I'd start with the type checker, restricted to the core language.

The parts that concern me the most right now are the individual compiler backends. They are tested, but I feel more needs to be done. I will continue to look for new approaches that increase confidence in the backends. Once the existing backends have been thoroughly vetted, I will start adding new backends on a regular basis.

When new models are released, I intend to give them a few hygiene tasks, such as running the `audit` skill. I think that may be quite a revealing experiment in its own right.

While it's likely that Kio still has bugs (almost all compilers do), I hope that Kio 0.1 meets reasonable standards. I'm excited to keep working on this language. In future posts I will be discussing the features that I believe make Kio stand out from other languages.

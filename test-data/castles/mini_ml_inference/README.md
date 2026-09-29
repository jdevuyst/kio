# Mini ML inference workbench

This program infers principal types for a small ML-like expression language.
It implements lexical scope, monomorphic lambda parameters, polymorphic
nonrecursive `let`, application, integer and Boolean literals, and pairs.
Type schemes quantify the variables free in the inferred value but absent
from the substituted environment. Each variable lookup instantiates its
scheme with fresh variables. Unification checks both constructor agreement
and the occurs condition.

`input.stdin` uses whole ASCII lines. Its first line is the number of cases.
Each case has a display-name line, a nonnegative token-count line, then
exactly that many token lines. A name following `var`, `lam`, or `let` is one
whole line, even if it contains spaces. Other token lines are these exact
prefix forms, whose operands appear on subsequent lines:

| Tokens | Expression |
| --- | --- |
| `int` | An integer literal |
| `bool` | A Boolean literal |
| `var`, name | A variable reference |
| `lam`, name, expression | A lambda |
| `app`, expression, expression | Function application |
| `let`, name, expression, expression | A nonrecursive let and its body |
| `pair`, expression, expression | An ordered pair |

Literal values are immaterial to inference, so `int` and `bool` carry only
their type. For example, four token lines `lam`, `x`, `var`, `x` encode the
identity function. Case framing lets a malformed expression be reported
without consuming the following case. The parser rejects missing operands,
unknown tokens, and extra tokens. Invalid framing ends the input report.

Each output line identifies the case and reports its type or its domain
error. Types use `Int`, `Bool`, `*` for pairs, and `->` for functions, with
explicit parentheses. Unsolved variables receive `a0`, `a1`, and so on in
first-occurrence order, independently of the inference engine's fresh-ID
counter. Domain errors are ordinary successful workbench output.

The fixture includes identity and composition, fresh uses of a let-bound
identity at different types, independent function instances in a pair,
lambda monomorphism, captured variables, a substituted environment before
generalization, shadowing, occurs failures, unbound names, constructor
clashes, and framing-preserving parse errors. Its expected types follow
directly from those expressions; they do not depend on internal variable IDs.

The modules separate prefix parsing and syntax, type traversal and display,
substitution unification, scheme/environment operations, Algorithm W, and
framed input. Persistent lists come from the reusable list package; direct
elaborator imports provide matching, conditionals, and sum construction.

What this adds to the corpus: a language-processing pipeline with lexical
schemes and environment-relative generalization, beyond first-order term
unification. Recursive syntax and type trees compose with persistent maps,
fresh-name state, fallible parsing and inference, and higher-order list walks
behind the ordinary compute host interface.

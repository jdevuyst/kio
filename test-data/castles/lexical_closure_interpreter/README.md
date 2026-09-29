# Lexical closure interpreter

A small call-by-value functional language implemented as a complete Kio
pipeline: line-delimited tokens, a prefix parser, a recursive expression tree,
lexical environments, first-class closures, an explicit continuation machine,
and a readable result report. Persistent lists from the reusable `list` package
hold token streams, environments, and continuation stacks. The `elab` package
supplies conditional, matching, and sum-construction forms.

`input.stdin` contains records: a descriptive title, an integer evaluation
budget, expression tokens one per line, and a line containing `end`. EOF ends
the record stream. The prefix grammar is:

```text
expression := integer
            | var name
            | fn name expression
            | app expression expression
            | add expression expression
            | let name expression expression
```

Each displayed word, name, or integer occupies its own line. Names are whole
lines consumed after `var`, `fn`, or `let`; `end` is reserved for record framing.
There is no seed. `let` is nonrecursive: its initializer runs in the original
environment and its body runs with the new binding. Application evaluates the
function and argument in caller scope, then evaluates the body in the closure's
captured environment extended by its parameter. Addition accepts integers.

The evaluator spends one fuel unit per continuation-machine transition,
including returning a value. Fuel exhaustion is a reported language error;
it bounds self-application without relying on native recursion. Parsing walks
the finite token stream and rejects missing expressions, unknown expression
tokens, and trailing tokens. Runtime checks report unbound variables and
invalid application or addition operands. These reports are successful runs
of the interpreter, so the castle exits zero.

Stdout repeats every title beside its value or error and finishes with the
program count. The lexical-shadowing program returns `15` from captured `x=10`
plus `5`, even though its caller binds `x=100`. The escaping closure keeps `7`
after its creating application returns, then adds `8`. A higher-order function
receives a doubling closure and produces `12`; another example checks that an
argument expression still reads the caller's environment. The self-application
fixture has only two small function bodies and a budget of thirty transitions.

What this adds to the corpus: lexical closure capture, an environment recursive
through first-class values, higher-order interpreted application, and explicit
continuation frames composed with a separate recursive prefix parser. It
exercises scope and lifetime behavior beyond arithmetic stack evaluation or
command/register interpreters, with parse, type, lookup, and fuel outcomes in
one bounded transcript.

# gate_symbolic_eval

This castle models a tiny arithmetic expression language with constants,
three fixed variables (`x`, `y`, `z`), addition, and multiplication. The program
builds several expression trees, renders each original tree, simplifies it with
rules for identities, zero annihilation, and constant folding, then evaluates
the simplified result in a fixed environment.

There is no `input.stdin`; the expression suite and environment are fixtures in
the Kio source. Stdout is a compact before/after/evaluation transcript for each
tree.

What this adds to the corpus: this is a medium symbolic simplifier/evaluator
castle. It stresses recursive newtypes over structural sums, labeled branches,
`match!` dispatch through the elaborator POC dependency, non-tail `rec(loop)`
tree walks, and the `testapi-arith-collection` host surface without using stdin.

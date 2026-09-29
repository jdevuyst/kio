# Boolean BDD workbench

This package compiles propositional expression trees into ordered, reduced
Boolean decision diagrams with variable order `x < y < z`. Node identifiers
refer to a persistent unique table. Equal children collapse immediately;
equal `(variable, low, high)` triples share the same node. A memoized binary
apply operation composes diagrams for conjunction, disjunction and exclusive
or. Negation applies exclusive or with the true terminal.

Seven formulas exercise distributivity, contradiction, tautology, shared
subgraphs, equivalence and a Boolean multiplexer. Restriction substitutes a
variable value, memoizes visited subgraphs and interns the remaining nodes.
Satisfying assignments follow a path through the reduced graph, preferring
false branches and assigning skipped variables false.

The fixtures are expression trees in `bdd/fixtures.kio`; there is no stdin.
Each output row contains the truth vector in binary `xyz` assignment order,
the number of satisfying assignments among all eight rows, the number of
reachable decision nodes (excluding the two terminals), and a satisfying
assignment or `none`. The expression-tree evaluator independently checks every
truth-table row and validates each reported witness. Contradiction is an
ordinary successful result with no witness.

The remaining lines check canonical root equality, terminal reduction,
allocation-free recompilation, commuted apply-cache reuse, restriction-cache
reuse, and the complete table's ordered, reduced and unique invariants.
The package depends on the shared List owner for tables, caches and graph
walks, and on the elaborator library for ordinary control and sum forms.

## What this adds to the corpus

This castle combines recursive expression trees with a canonical shared graph,
persistent interning, two memoized graph algorithms, model extraction and an
exhaustive bounded semantic oracle. Its graph identity and sharing checks make
the reduction algorithm observable alongside its Boolean answers.

# Exact linear-system workbench

This workbench row-reduces small integer linear systems using normalized exact
rational numbers. It scans columns for pivots, swaps rows when necessary,
normalizes each pivot, eliminates that column from every other row, and compares
the coefficient and augmented ranks. Consistent systems produce a particular
solution and one nullspace direction for each free variable. A separate module
substitutes those vectors into the original equations. It also perturbs a pivot
coordinate to demonstrate that substitution rejects a wrong witness.

`input.stdin` uses one complete ASCII token per line. Each system consists of
its name, its equation count, its variable count, and its augmented matrix in
row-major order. Each matrix entry occupies its own line: all coefficients of
the first equation, then its right-hand side, then the next equation. Counts
are between one and three; entries are integers between -8 and 8. A line `END`
ends the workbench. This framing is deliberate: no whitespace splitting or
substring host capability is required. Input errors are reported as domain
messages. There is no random seed.

The five fixtures cover a unique fractional solution requiring a row swap, a
rank-one system with two free variables, a skipped leading pivot column,
inconsistent redundant equations, and an all-zero system. Their small values
keep the exact arithmetic within I32 bounds. The report shows original and
reduced augmented rows, row-swap counts, both ranks, classification, solution
vectors, and exact residuals. Columns and direction numbers in the report are
one-based. A family means `particular + t1 * direction1 + ...`, with arbitrary
rational parameters. All residual entries must be zero for a valid witness.

The package uses the List library for persistent matrices and vectors and the
elaborator library for ordinary conditionals and sum dispatch. A local adapter
rehosts list traversal onto the `testapi-compute-list-elab` runner contract;
integer equality is ordinary Kio code composed from comparisons. Its modules
separate input framing, rational arithmetic, matrix operations, reduction,
solution-family construction, substitution checks, and reporting.

What this adds to the corpus: exact linear algebra with real pivot search and
row permutation, rank-deficient and inconsistent branches, multi-vector
solution families, and an independent original-equation oracle inside the
program. The persistent nested-list matrix transformations and stdin-driven
dimensions exercise composed algorithms beyond standalone rational-expression
evaluation.

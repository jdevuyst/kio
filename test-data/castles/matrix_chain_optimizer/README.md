# Matrix-chain optimizer

This program chooses how to parenthesize an ordered chain of matrix
multiplications. It fills a table of interval costs and split choices in
increasing interval width, then reconstructs an actual recursive tree from
those choices. Equal-cost candidates keep the earliest split.

The fixtures are in the program; there is no stdin. The main chain has shapes
5x10, 10x3, 3x12, 12x5, and 5x8. Its unique optimum splits after A2 and costs
570 scalar multiplications, compared with 830 for a left fold and 1408 for a
right fold. Three 4x4 matrices exercise a tie: both plans cost 128, and the
earliest-split policy chooses `(A1*(A2*A3))`. A single matrix costs zero.

An independent tree replay checks every internal dimension and consecutive
leaf ordering while recomputing output dimensions, scalar cost, and leaf
count. Its result is printed alongside the table cost and the two baseline
trees' replayed costs. Matrix numbers and root splits are one-based; a root
split of zero denotes a leaf. A reversed square-matrix tree demonstrates that
replay rejects an ordering error even when dimensions compose.

Empty chains, incompatible neighboring matrices, and zero dimensions produce
readable rejection messages while the program succeeds. Accepted chains
contain one to eight matrices with dimensions in 1..30, bounding every cost
by 189000 and keeping all table arithmetic within i32.

`chain.kio` owns shapes and input validation, `optimizer.kio` owns the dynamic
programming table and reconstruction, and `tree.kio` owns tree rendering,
replay, and fold baselines. The main module assembles fixtures and reports
their results. The package uses the shared elaborator library and the
`testapi-array` host protocol.

What this adds to the corpus: interval dynamic programming whose output is a
recursive expression tree, with separate dimension-and-cost replay and
deterministic ties. Mutable array tables compose with labeled records,
recursive sums, imported matching and conditionals, and both tail and
continuation recursion.

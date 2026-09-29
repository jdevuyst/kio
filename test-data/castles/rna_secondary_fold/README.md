# RNA secondary folding toy

A combinatorial toy over RNA-letter sequences, not biological prediction.
The model admits only A–U and G–C pairs in either direction, gives each pair
one point, and forbids crossing pairs or reuse of a position. A minimum loop
parameter requires at least that many sequence positions strictly between
paired endpoints; those positions may themselves participate in nested pairs.
There are no energies, chemical conditions, or biological claims.

The fixture sequences live in `workdir/fold/fixtures.kio`; this program reads
no stdin. The host supplies mutable arrays and elementary arithmetic, text,
and looping operations through `testapi-array`. The direct `elab` dependency
supplies ordinary control and structural elaborators.

`solver.kio` fills interval scores in increasing span order. For an interval
`[i,j]`, it compares leaving `i` unmatched with pairing `i` to each compatible
`k`, combining the optimal interior `[i+1,k-1]` and suffix `[k+1,j]`.
Strict improvements replace the recorded decision: ties retain the unmatched
option first, then the earliest equally good partner. The table costs
O(n²) space and O(n³) time.

`plan.kio` represents decisions as named sum alternatives. `reconstruct.kio`
walks those decisions with an explicit interval stack, builds a reciprocal
partner array, and renders dots for unmatched bases and parentheses for pair
endpoints. `validate.kio` independently scans the reconstructed array for
pair legality, reciprocal links, crossings, stack-consistent bracket closure,
and agreement between counted pairs and the claimed score. It never consults
the dynamic-programming table or its decisions.

Stdout prints each sequence, its minimum loop size, dot-bracket fold, and
pair count. `optimum=true` compares with the fixture's independently derived
maximum. The other Boolean fields describe the reconstruction checks.
The cases include an empty sequence, an unpairable sequence, a two-pair
hairpin, four nested pairs, two separate stems, and a stricter loop bound.

`ACUG` makes the crossing restriction observable: `(0,2)` and `(1,3)` are
individually legal but cross, so the maximum is one, rendered `.(.)`.
`GGGAAACCCUUU` has six possible disjoint pairs if crossings are allowed, but
its best noncrossing score is three. Its G–C and A–U blocks cannot both
contribute without a crossing. The two separate stems in `GCAAAGCAUAAAU`
produce `((...))((..))` with four pairs, exercising both recursive subintervals.

The final audit examples deliberately supply crossed pairs, incompatible
bases, a too-short loop, a one-sided link, and an incorrect claimed score.
Their expected false fields demonstrate that the validators reject those
specific defects; the program as a whole succeeds.

What this adds to the corpus: interval dynamic programming with a retained
decision table, stack-based reconstruction, and independent structural
validation over mutable arrays. The sequence domain combines nested and
disjoint subproblems with a crossing-versus-noncrossing choice, while imported
`if!`, `match!`, and `widen_sum!` compose across the domain modules.

# Suffix automaton index

An online substring index over arrays of integer symbols from the alphabet
`0, 1, 2`. Each appended symbol extends a suffix automaton, walks suffix
links to add missing transitions, and splits states when two continuation
classes require different maximum lengths. Split states own cloned mutable
transition rows and start with zero endpoint weight. A final descending-length
pass propagates occurrence counts through suffix links.

The package has no stdin. Its bounded fixtures are `0 1 1 0 1 0 2`, the
overlapping word `0 0 0 0`, and an empty word. The first fixture creates two
clones; later transitions make both clone rows differ from their source rows.
Queries include whole words, single symbols, overlapping substrings, an absent
word, an out-of-alphabet symbol, an overlong word, and the empty word. Empty
queries count the `length + 1` boundaries between symbols.

Stdout reports state and clone counts, distinct nonempty substrings, the longest
repeated substring length, and query occurrence counts. A separate direct scan
enumerates every nonempty interval, compares its count with the index, and
independently calculates distinct and repeated-substring metrics. It advances
candidate start positions one at a time, so overlapping matches count. A zero
mismatch total certifies all present query words in each fixture; explicit
queries also exercise rejection and boundary cases.

The automaton, finalized query analysis, exhaustive scan, and reporting are
separate modules. The shared `elab` dependency provides ordinary `if!` control
forms; the exact `testapi-array` protocol supplies arithmetic, mutable arrays,
printing, and the loop capability.

What this adds to the corpus: online end-position class refinement with suffix
links, zero-weight clones, independently mutable transition copies, and a
separate occurrence-propagation phase. This composes mutable state indexing and
nested loop-driven walks around a substring-query workload, with exhaustive
direct enumeration as an independent oracle.

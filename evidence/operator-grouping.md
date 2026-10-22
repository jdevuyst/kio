# Operator grouping arguments

These arguments explain the structural checks in
[`fixed_operators.rs`](../kio-rs/src/pass/parser/fixed_operators.rs) and the
operator-pattern validator in
[`parser/core.rs`](../kio-rs/src/pass/parser/core.rs).
The [operator contract](../specs/language.md#operators) is normative.
The arguments are not a machine-checked proof or a whole-parser complexity bound.

## Scope of the classifier

A fixed pattern alternates value slots and contiguous fixed words, contains
at least one of each, and has no implicit grouping. `_` admits an atom, `__`
also admits a chain rooted at the same locally selected operator, and `___`
admits an operator chain. A fixed final word completes an atom. An operand
followed by a fixed word ends at that word; the same operator has no exception
to this boundary. Explicit grouping bounds both left and right chain extent.

An operand-ending non-prefix pattern cannot have chain-enabled slots at both
exposed ends. This excludes competing left/right claims in its own ungrouped
repetition, independently of whether those slots use `__` or `___`. Interior
slots have a following fixed boundary, and a final fixed word closes the result;
neither creates a second exposed end.

Subject to those restrictions, replacing a particular `__` by `_` leaves its
syntactic permission unchanged exactly in the following cases:

1. **The pattern ends in a fixed word.** Every complete self-child is already
   an atom and is therefore admitted by `_`. A partial self-child is not a
   complete operand. Other chain-enabled slots do not change this endpoint.
2. **The pattern is operand-ending and non-prefix, and the selected nonleading
   slot's following word equals its first fixed word.** That pending word ends
   the operand before it can introduce a bare self-child. Parenthesizing the
   child makes it an atom, again admitted by `_`.

The remaining positions have constructive witnesses. Fill every other slot
with a fresh native value atom and put exactly one bare self-child in the
selected slot. For a prefix pattern, its initial word starts that child. For a
leading continuation slot, the child is the preceding same-operator chain.
For a nonleading continuation slot, the child's first word differs from the
pending boundary, or there is no pending boundary. The child's own operands
are atoms. The exposed-end restriction prevents an opposing self-claim at the
other end. The selected `__` admits the child, whereas `_` rejects it.

This construction does not depend on a bound on the number of slots. Other
`__` and `___` slots contain atoms in the witness, so neither adds a competing
chain nor changes the selected child's identity. The claim concerns grammar
permissions, not whether an arbitrary implementation function has a type that
allows every self-nested application.

The generated `each_admitted_self_slot_has_a_bare_self_witness` test checks a
finite domain of permission vectors, prefix/continuation shapes and repeated
words. It checks both the admitted self-child and rejection after weakening
that slot. Definition tests cover the two ineffective cases and opposing ends.
These executable witnesses supplement, rather than establish, completeness.

## Grouping precedes admission

The parser records unfinished operator frames and the innermost pending fixed
boundary. A leading chain slot selects its preceding extent using those frames
and the declared permission, before checking the crossed operands. Competing
incoming-left and outgoing-right claims reject. A failed operand check never
retries a shorter extent or selects a different tree.

For `_ + _` and `___ < _ >`, `a + b < k >` therefore groups as
`(a + b) < k >`. With `_ $ ___` instead, `a $ b < k >` has opposing claims
and needs parentheses. Both explicit alternatives remain distinct trees, and
formatting must preserve that distinction. The whitespace/formatter tests in
[`operator_whitespace.rs`](../kio-rs/tests/operator_whitespace.rs) compare
ordinary lowered call trees, not only successful parsing or idempotence.

## Provider independence

Square-frame identity follows the source's structural brackets and mirrored
marker words before binding lookup. Fixed words are not split according to
available shorter bindings. The consumer writes its complete selected operator
grammar; provider resolution checks that exact export rather than searching
for a usable parse or implementation type. Adding unrelated declarations to a
provider therefore cannot change an unchanged consumer's frame, grouping or
selected grammar. This argument does not claim a wider guarantee for arbitrary
changes to the consumer's imports or literal-role choices.

## Formatter boundary reuse and measured cost

The formatter's [`OperatorBoundaryCache`](../kio-rs/src/pretty.rs) memoizes
grouping decisions during one operator-chain layout. Slot queries retain the
expression, parent-pattern identity and slot position. Edge queries retain the
expression, boundary direction and effective semantic target: a following word
by content, a recursive pattern by words and slot kinds, or the shared
greedy-only category. Equal patterns with different source spans therefore
reuse an edge answer. The cache is transient and shared through segment
construction; separately laid-out nested chains can start separate caches.
It neither changes grouping nor consults providers.

`operator_boundary_cache_uses_semantic_contexts` checks that equal pattern and
word contents reuse entries, then bounds retained edge/slot queries for four
chain families at sizes 32, 64 and 128. These deterministic checks and the
grouping/formatting witnesses establish firing and behavior; elapsed time is
not a correctness oracle or a general complexity proof.

Final optimized measurements use revision
`3400e4ebcf5b789092522fef781a48b6690d25bb`. The before-reuse debug column
measures the earlier implementation of the same boundary rules before shared
memoization. All batches used `kio fmt -`, with a generated module on stdin
and captured stdout/stderr.
Each cell is the median of three separate process wall times, measured with a
monotonic clock; process startup and system load are included. A 20-second
per-process timeout was a hang safeguard. Sizes were 16, 32, 64, 128 and 256
for seven shapes. The optimized column uses the ordinary corpus compiler
configuration: package opt-level 2, with development assertions and overflow
checks enabled. It is not a release-profile comparison.

At size 256, times are milliseconds:

| Shape | Before reuse, debug | Final debug | Optimized | Formatted bytes |
|---|---:|---:|---:|---:|
| Repeated prefix | 4,098.8 | 18.9 | 10.4 | 1,591 |
| Same-operator right chain | 8,561.0 | 27.2 | 11.3 | 3,132 |
| Distinct prefixes | 5,020.9 | 81.7 | 22.3 | 4,552 |
| Wide strict pattern | 31.0 | 17.5 | 8.9 | 4,167 |
| Wide pattern with prefix operands | 45.9 | 27.6 | 7.8 | 5,206 |
| Alternating right chain | — | 232.7 | 40.8 | 166,603 |
| Distinct-operator right chain | — | 237.4 | 39.0 | 203,968 |

Repeated-prefix and same-operator-right debug costs fell approximately 217×
and 315×. Final debug and optimized formatting agree byte-for-byte across all
35 shape/size pairs, checked by lengths, SHA-256 digests and direct reference
comparisons. This bounded output agreement supplements the semantic tests.

An observed older debug binary took 34.9 ms and 45.1 ms for the two mixed-right
rows, respectively: final debug is still approximately 6.7× and 5.3× slower.
That older binary has no authenticated source revision, so it establishes
neither a comparison with an exact main revision nor a release baseline.
Its mixed-row output lengths and digests are identical too; the large,
quadratically indented output does not explain the relative slowdown.
Boundary analysis retains mixed-chain overhead. The largest optimized median
was 40.8 ms for these size-256 examples; this is an observation, not a time
bound for arbitrary programs. No all-shapes linearity or baseline-performance claim
follows from this measurement.

### Reproducing the shapes

Use `module scale; import ops(PATTERNS); fn run() { EXPRESSION }` as the input
to `kio fmt -`. Provider files and typechecking are unnecessary for formatting.
In the following recipes, repetition expands to separately written words and
operands, never to a split symbol word:

- Repeated prefix: import `op ! ___`; write `!` N times before `value`.
- Same-operator right chain: import `op _ + __`; join N+1 `value` operands
  with `+`.
- Distinct prefixes: import `op W_i ___` for each i from 0 to N−1; write
  `W_0 … W_(N−1) value`.
- Wide strict pattern: import `_` followed by N copies of `? _`, then `: _`,
  tagged with `op`; write `value` followed by N copies of `? value`, then
  `: value`.
- Wide prefix operands: change those N middle slots to `___`, additionally
  import `op ! ___`, and use `? ! value` for each middle operand.
- Mixed right chains: import `op _ W_i ___` for the distinct words used;
  write `value W_0 value … W_(N−1) value`. Alternate two words for the
  alternating shape, or use N distinct words for the distinct shape.

For exact words, prefix `~` to the least-significant-digit-first base-13
representation of i over `!?$%&*+-<>^|~`, retaining one digit for zero:
`W_0 = ~!`, `W_1 = ~?`, `W_13 = ~!?`. Run all sizes under the same build
configuration and compare output bytes separately from timing.

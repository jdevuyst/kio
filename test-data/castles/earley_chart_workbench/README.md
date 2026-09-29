# Earley chart workbench

This program recognizes numeric token sequences with context-free grammars.
It builds an Earley chart with one column per token boundary. Each item is a
production index, a dot position within that production, and an origin
column. Prediction adds rules for the next nonterminal; scanning advances
over a matching token; completion advances items waiting in the origin
column for a finished nonterminal.

Every column is a set of unique items. Closure repeatedly sweeps the current
column until no new item is inserted. In particular, a completed nullable
production is revisited when another item begins waiting for it. This makes
completion independent of insertion order. Left recursion, empty
productions, and mutually recursive unit productions all terminate because
each finite chart admits only finitely many distinct items.

The fixed grammar and token arrays live in `workdir/earley/fixtures.kio`.
There is no stdin or text parser: numeric symbol identities are direct input
to the recognition algorithm. Nonterminals and terminals use distinct label
arms, so their numeric IDs may overlap. All examples start at nonterminal 0.

| Suite | Grammar | Token IDs |
| --- | --- | --- |
| `nullable` | `S -> A A`, `A -> epsilon \| a` | S=0, A=1; a=1 |
| `nullable-reordered` | The same productions in reverse order | Same |
| `balanced` | `S -> S S \| ( S ) \| epsilon` | S=0; opening=1, closing=2 |
| `cyclic` | `S -> A B`, `A -> B`, `B -> A \| epsilon \| b` | S=0, A=1, B=2; b=3 |

`nullable` recognizes zero, one, or two `a` tokens and rejects three. Empty
input requires the second `A` to consume an already-known empty completion.
The reordered suite checks the same cases after reversing the production
array. `balanced` accepts empty, nested, and concatenated parentheses and
rejects an unmatched opening and a closing-before-opening sequence. Its
left-recursive concatenation rule and nested pair rule exercise context-free
recognition. `cyclic` recognizes zero, one, or two `b` tokens while its unit
cycle exercises duplicate suppression and nullable completion.

Output gives the suite, readable input, acceptance decision, and the number
of unique items in each closed chart column. For example, `chart=[5,5,2]`
means five states before input, five after the first token, and two after the
second. The empty-input nullable chart has five items, including both
successive completions of the two `A` occurrences. Dead prefixes retain
empty later columns. A rejected input is an ordinary successful workbench
result; the package ends after 18 recognitions.

Acceptance expectations follow the grammar languages. A bounded derivation
oracle independently enumerates the languages through length four; a
separate relational Earley closure supplies the exact chart counts. The
inputs have at most four tokens and the grammars at most five productions. The array-literal
helper, typed grammar model, deduplicated chart storage, closure operations,
recognition, fixtures, and reporting are separate modules. Direct elab
imports supply ordinary conditionals, matching, and sum construction.

What this adds to the corpus: context-free chart recognition with nullable
and left-recursive fixed points, production-order invariance, and a
mutually nullable cycle. Tagged grammar symbols and nested mutable chart
arrays compose with generic array literals and loop-driven set operations;
the chart counts expose incomplete closure even when a verdict agrees.

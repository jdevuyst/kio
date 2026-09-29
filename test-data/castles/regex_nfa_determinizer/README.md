# regex_nfa_determinizer

A regular expression compiled into an automaton, three stages deep. Four
patterns over the three-letter alphabet `{a, b, c}` go through Thompson's
construction, the subset construction, and Moore's partition refinement; six
probe words are then recognized against each minimized automaton.

There is no `input.stdin`. The patterns and the probe words are fixtures in
the Kio source (`automata/suite`), and they arrive as **abstract syntax**, not
as text — this castle is about what happens *after* parsing, so it has no
lexer.

## The patterns

| # | Pattern | Why it is here |
| --- | --- | --- |
| 1 | `(a\|b)*abb` | The textbook case. Fourteen NFA states collapse to six DFA states, and the refinement then finds that "nothing seen yet" and "the last letter was a b" are the same state — a `b` makes no progress toward a literal that begins with an `a`. |
| 2 | `ac\|bc` | Two branches that converge. Determinization keeps them apart, because the two `c` edges leave different NFA states; only the refinement sees that both branches accept the same suffix. It merges twice. |
| 3 | `a(b\|c)+` | One or more letters drawn from a two-letter class. |
| 4 | `(ab)?c*` | An optional prefix in front of a star. Its start state accepts, which is the case a refinement that assumed otherwise would get wrong. |

Between them the four use all six node kinds of the pattern syntax — a letter,
concatenation, alternation, `*`, `+`, and `?`.

## The three stages

1. **Thompson's construction** (`automata/nfa`) walks the pattern tree and
   emits a nondeterministic automaton with epsilon transitions. Every fragment
   it builds has exactly one entry and one exit; a letter mints two fresh
   states, and each operator mints its own entry and exit around what it
   wraps. States are numbered in source order, so the numbering is
   reproducible from the pattern alone.
2. **The subset construction** (`automata/subset`) determinizes it. A DFA state
   is a *set* of NFA states — those the NFA could be in after reading the input
   so far — closed under epsilon edges. A worklist interns every closure it
   meets, so two moves landing on the same set share one DFA state. That
   interning is the whole of the merging determinization does.
3. **Moore's partition refinement** (`automata/minimize`) merges the DFA states
   no word can tell apart. It starts from the only distinction visible without
   reading anything — accepting versus not — and splits a block whenever two of
   its members send some letter into different blocks.

The DFA is **total** over `{a, b, c}`: a pattern that never mentions a letter
still sends it somewhere, to the dead state the subset construction mints for
the empty set. That is why every automaton below carries one non-accepting
state that loops to itself on every letter, and why a pattern over only two
letters still needs three columns.

## Reading the output

Per pattern, stdout carries:

- the pattern **rendered back to regex notation** from its syntax tree, with
  parentheses only where a reader would write them;
- `nfa states`, `dfa states`, and `min states` — the last with the number of
  states the refinement merged away;
- `subset states`, listing the NFA state set behind each DFA state. This is the
  evidence for what determinization merged: DFA state `0` of pattern 1 is
  `{0,2,4,6,7,8}`, six NFA states in one;
- the **minimized transition table**, and
- the **recognition verdicts** for the six probe words.

A transition table row reads:

```text
      st     a     b     c
  >    0     1     0     2
   *   4     1     0     2
```

The `st` column is the state number; each letter column is the state that
letter leads to. `>` marks the start state and `*` an accepting one — a state
can be both, as state `0` of pattern 4 is. So minimized state `0` of pattern 1
goes to state `1` on an `a`, back to itself on a `b`, and to state `2` (the
dead state) on a `c`.

## The correctness argument

Determinization and minimization each rewrite the machine wholesale, so the
recognition block supplies regression evidence for the other two stages. Every
one of the six probe words is run against every pattern's **minimized**
automaton, and each of the twenty-four selected accept/reject cells is fixed in
advance by what the pattern *means*:

| probe | `(a\|b)*abb` | `ac\|bc` | `a(b\|c)+` | `(ab)?c*` |
| --- | --- | --- | --- | --- |
| `""` | reject | reject | reject | **accept** |
| `"abb"` | **accept** | reject | **accept** | reject |
| `"aabb"` | **accept** | reject | reject | reject |
| `"ac"` | reject | **accept** | **accept** | reject |
| `"bc"` | reject | **accept** | reject | reject |
| `"abcc"` | reject | reject | **accept** | **accept** |

A regression that changes any of these selected words flips a cell. The probes
are deliberately mixed — every probe is accepted by at least one pattern and
rejected by at least one, and every pattern accepts at least two and rejects at
least three — but they are a finite regression set, not a proof over every word.

The headline numbers, all hand-checkable against the printed state sets:

| # | Pattern | NFA | DFA | minimized |
| --- | --- | --- | --- | --- |
| 1 | `(a\|b)*abb` | 14 | 6 | 5 |
| 2 | `ac\|bc` | 10 | 6 | 4 |
| 3 | `a(b\|c)+` | 10 | 5 | 4 |
| 4 | `(ab)?c*` | 10 | 5 | 4 |

## Modules

- `automata/seq` — cons lists, and sets of states as strictly ascending integer
  lists. This tier hands the package no array, so every collection is one of
  these; keeping a set sorted is what makes "have I built this DFA state
  already?" a plain elementwise comparison.
- `automata/symbol` — the alphabet, and epsilon as the code one past it.
- `automata/pattern` — the pattern syntax as a labels sum grounded in a
  `newtype`, its constructors, and the precedence-aware renderer.
- `automata/nfa` — edges, automata, and Thompson's construction.
- `automata/dfa` — the deterministic automaton and its flat transition table,
  plus recognition.
- `automata/subset` — epsilon closure and the subset construction.
- `automata/minimize` — Moore's partition refinement.
- `automata/report` — the aligned tables. The host surface can print a string
  and stringify a number but cannot *measure* a string, so column widths are
  computed arithmetically, from a number's decimal width or a word's letter
  count.
- `automata/suite` — the four patterns and the six probes.

What this adds to the corpus: the corpus's first automata compiler — Thompson
NFA construction from a pattern AST, epsilon-closure subset construction to a
DFA, and partition-refinement minimization, with twenty-four selected
recognition results as regression evidence for language preservation. It is
the first castle to carry three composed graph-rewriting algorithms in one
pipeline and to use a `fold` over a polymorphic cons list (a higher-order
function value threaded through a `rec(loop)` group) alongside specialized
direct-recursive traversals. It is also the first whose data model is
sets-of-sets — DFA states are sorted integer lists, interned by structural
equality, and the interning is what does the work.
`tiny_regex_engine` matches a string against a regex directly; this castle
never matches anything against a pattern — it compiles the pattern into a
machine, twice, and then runs the machine.

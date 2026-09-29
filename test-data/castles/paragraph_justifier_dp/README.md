# paragraph_justifier_dp

An optimal line-breaking engine, set head to head against the greedy
first-fit it is supposed to beat.

The program takes one measured paragraph and one column width, breaks the
paragraph into lines twice — once greedily, once by dynamic programming over
break points — prices both layouts on the same badness scale, and reports the
difference. The greedy answer costs **449**; the optimal answer costs **377**.
That gap is the castle.

There is no `input.stdin`. The paragraph lives in `justify/paragraph.kio` as
ordinary source data.

## The paragraph and the column

Thirteen words, set to a column of **16**, with one space in every gap:

```text
typesetting is the art of breaking a paragraph into lines that read evenly
```

Each word is a `(text, width)` record because typesetting operates on measured
glyph widths. A typesetter reads a width out of the font's metric table rather
than counting glyphs again at every candidate break. Here every width happens
to equal its text's character count, so the margin the transcript draws is the
real margin and the ragged right edge you see is the slack the cost was
computed from.

## The badness rule

A line holding words `i .. j - 1` occupies

```text
used = (sum of those words' widths) + (one gap between each adjacent pair)
slack = column - used
```

and costs

- **`infinite`** if `used > column` — a line that runs past the margin is not
  a line. `infinite` is a sentinel (`1000000`), larger than any layout of this
  paragraph can cost and small enough that two of them still add up inside
  `i32`. Costs saturate rather than wrap, so an overfull candidate competes in
  the same `min` as every other and simply always loses. It is not a separate
  arm of a sum, and it never reaches the transcript — its footprint there is
  the `fits` column of the DP table.
- **`0`** if this is the **last line** of the paragraph, and it fits. A
  paragraph ends where it ends; its final line is short for a reason no
  typesetter can fix, so charging for that shortness would price a fact rather
  than a choice.
- **`slack^3`** otherwise. Cubing is the whole reason a global optimum is
  worth computing: one badly loose line costs far more than several slightly
  loose ones, so an algorithm that can trade the two comes out ahead of one
  that cannot.

Freeing the last line is a typesetting convention, not what makes the
optimization global. Each earlier break changes both the current line's slack
and the choices available to the suffix, so dynamic programming is needed
under either scoring rule. If the final line were charged like a body line,
this fixture would still cost **717** optimally versus **1449** greedily. The
documented free-last-line rule excludes that final raggedness and yields the
reported totals of **377** and **449**.

## The two algorithms

**Greedy (first-fit)** takes words while they fit and breaks before the first
one that does not. It never looks past the word in front of it.

**Optimal (dynamic programming)** treats the paragraph as `n + 1` break points
and finds a shortest path through them:

```text
best[j] = min over i < j of ( best[i] + badness of the line i .. j - 1 )
```

`best[j]` is the cheapest way to set the first `j` words as whole lines;
`back[j]` remembers the `i` that achieved it, so the layout can be read back
out of the table afterwards.

Both algorithms answer with the same artifact — a **break list**, the indices
`0 .. n` at which lines start — so the transcript can lay their results out
side by side and price them with one shared measuring routine.

## How to read the DP table

One row per break point `j`:

| column | meaning |
| --- | --- |
| `best` | the cheapest total cost of setting words `0 .. j - 1` |
| `back` | the start of the last line in that cheapest layout |
| `fits` | how many of the `j` candidate starts produced a line that fit at all — the rest scored `infinite` and were never in the running |

Break point `0` is the empty prefix. It costs nothing, has no predecessor, and
has no candidates to count, so its `back` and `fits` print as `-`.

The scan for each `j` walks the candidate start downward from `j - 1` to `0`,
and a candidate must be **strictly** better to displace the incumbent — so a
tie keeps the **largest** start, the shorter of the two lines. Rows `7` and
`11` are genuine ties (`368` from starts `5` and `4`; `592` from starts `9` and
`8`), and they are why those `back` entries read as they do.

## What the optimum buys

Read the two layouts against each other and the whole difference is in the
first two lines:

```text
greedy    |typesetting is  |  slack 2  badness   8      \  224
          |the art of      |  slack 6  badness 216      /

optimal   |typesetting     |  slack 5  badness 125      \  152
          |is the art of   |  slack 3  badness  27      /
```

Greedy fills the opening line as tightly as it can and is immediately punished:
having taken `is`, the next line can only reach `the art of` and leaves six
columns bare, which cubes into 216. The optimum **gives the opening line
away** — `typesetting` alone, five columns of slack, 125 — and buys a second
line that is three columns short instead of six. It spends 152 where greedy
spends 224, and the two layouts agree on every line after that. Optimal beats
greedy by **72**.

## What stdout means

- `metrics` — the paragraph as the typesetter holds it: every word beside the
  width it was measured at.
- `greedy (first-fit)` / `optimal (dp)` — the chosen break list, then the
  paragraph as it would be set, between two rules drawn at the column. The gap
  between a line's last glyph and the right rule *is* its slack. Each line
  reports the words it spans, its used width, its slack, and its badness; the
  block ends with the layout's total.
- `dp table` — the table above.
- `verify` — the optimal layout's cost, **re-summed from the recovered lines**,
  against the DP's own `best[n]`. This is a real check, not a restatement: the
  DP accumulates a line's width leftward as it extends a candidate, while the
  layout measures every recovered line again from the paragraph, from scratch.
  A back-pointer that walks to the wrong predecessor, or a scan whose running
  width drifts, makes the two numbers disagree. They agree at 377.
- `comparison` — both totals, the delta, and the verdict `optimal <= greedy`,
  which is a theorem about the DP (greedy's break list is one of the layouts it
  searched) and prints as `true`.

## What this adds to the corpus

The corpus's first global-optimization DP: optimal line breaking with a
back-pointer recovery, set head-to-head against a greedy first-fit that it
provably beats, with the recovered layout's cost re-summed to catch a
back-pointer bug. Where `edit_distance_audit` fills a DP table over host arrays
and reads one number out of the corner, this one has to *reconstruct the
decision path* — and it carries a second, deliberately weaker algorithm
alongside the optimal one purely so the transcript can prove the optimum is
worth its table. It is also the corpus's first DP that keeps its whole table in
a Kio cons list: the `testapi-arith-collection` tier has no host array, so the
three table columns are lists grown head-first in exactly the order the inner
scan reads them back, and random access into them is a walk. The cost model
adds a saturating `infinite` sentinel over `i32` (no division, no wrapping) and
the conventional free-last-line treatment for a paragraph's natural ending.

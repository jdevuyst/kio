# segment_tree_lazy

A segment tree with **lazy propagation** over 64 integers, replayed
side by side against a brute-force array that computes the same answers
the obvious way.

The program reads no input — the initial array and the eighteen-operation
script are fixture data in `seg/script.kio`, so there is no `input.stdin`.
Everything the run does is on stdout.

## The tree

The array has 64 cells, indices `[0..63]`. The tree over it is a flat heap:
node `i`'s children are `2i + 1` and `2i + 2`. A node is never told its own
range — the descent threads `(lo, hi)` down and splits at
`mid = lo + half(hi - lo)`, so the left child takes `[lo, mid]` and the right
child `[mid + 1, hi]`.

That makes the top of the tree easy to name, and the script leans on it:

| node | range | node | range |
| --- | --- | --- | --- |
| 0 | `[0,63]` | | |
| 1 | `[0,31]` | 2 | `[32,63]` |
| 3 | `[0,15]` | 4 | `[16,31]` |
| 5 | `[32,47]` | 6 | `[48,63]` |

The host env this castle runs under has **no division and no `<`** — the only
integer comparison it supplies is `leq_i32`. So `seg/num.kio` derives equality
and strict order from `<=`, and derives integer division by a doubling search
(`idiv`, and `half` on top of it). The tree needs a midpoint at every split;
that is where the division goes.

## The two aggregates

Every node stores an aggregate over its range, and the aggregate has **two**
components (`seg/agg.kio`):

- `sum` — the total of the cells in the node's range;
- `min` — the smallest cell in the node's range.

Two are the point. A structure carrying only `sum` would make the lazy
arithmetic look like a coincidence; carrying `min` as well shows what the
pending-update arithmetic really has to do, because the two components react
to a range-add *differently*:

```text
shift(agg, len, x)  =  { sum = agg.sum + len * x,  min = agg.min + x }
```

A pending "add `x` to each of my `len` cells" moves the sum by `len * x` and
the minimum by only `x`. Both are computable **from the aggregate alone** — no
leaf has to be read. That single fact is what the whole data structure is
built on.

The tree keeps these in three parallel `Array(I32)`s: `sums`, `mins`, and
`tags`.

## What a lazy tag is, and when it gets pushed

`tags[i]` is an add that has been applied to node `i`'s *own* aggregate but has
**not** reached node `i`'s children. That is the invariant every function in
`seg/tree.kio` preserves:

> node `i`'s aggregate is already correct for its whole range,
> but `tags[i]` is an add that has not reached its children yet.

Two moves maintain it:

- **`apply_tag(node, lo, hi, x)`** — record "add `x` to every cell of
  `[lo, hi]`" at `node` *without descending*. The node's own `sum` and `min`
  are fixed by the arithmetic above, and the add is parked in `tags[node]` for
  the children to collect later. Leaves have no children, so they store no tag.

- **`push_down(node)`** — hand `tags[node]` to both children (via `apply_tag`
  on each) and clear it. This is the **only** place a tag ever moves, and a
  descent calls it in exactly one situation: it is about to look *below*
  `node`. A zero tag is not a push — there is nothing to hand down.

So a range-add walks down from the root, and at each node does one of three
things: the node is **disjoint** from the range (do nothing); the node is
**covered** by the range (`apply_tag` and *stop* — an entire subtree is
updated by touching one node); or neither, in which case it must
`push_down` and recurse into both children. A query is the same walk, and it
pays the deferred bill: a query that descends through a node with a pending
tag pushes that tag one level down before it can read the children.

## The script

Eighteen operations, replayed against both structures in lockstep
(`seg/driver.kio`). Both start from the same 64-cell fixture — a sawtooth that
scatters the minimum around the array rather than parking it at one end.

| # | operation | why it is in the script |
| --- | --- | --- |
| 1–2 | `sum[0,63]`, `min[0,63]` | baseline: the root covers the whole range, so 1 visit each |
| 3 | `add[0,31] +3` | **covers node 1 exactly** — one tag, no descent |
| 4 | `sum[0,31]` | answered *at* that same covering node — still no descent, no push |
| 5 | `min[16,23]` | must walk through node 1: **two tags come down** |
| 6 | `add[32,63] +2` | **covers node 2 exactly** |
| 7 | `sum[32,63]` | answered at node 2, which is holding a pending tag |
| 8 | `min[0,63]` | answered at the root, whose children both hold pending tags |
| 9 | `add[8,55] +5` | ragged range: settles on several covering nodes, pushes 3 |
| 10 | `sum[8,55]` | same walk again — and now **0 pushes**: that bill was already paid |
| 11 | `set[20] := 100` | one leaf, but the whole path down to it must be pushed first |
| 12–13 | `sum[0,63]`, `min[0,63]` | root again |
| 14 | `add[0,63] +4` | **covers the root** — the entire array updated at *one node* |
| 15 | `sum[0,63]` | 1 visit, and it already sees the +4 |
| 16 | `min[24,39]` | straddles the root's split, so the root's tag finally moves |
| 17 | `add[0,63] -2` | the root again, negative this time |
| 18 | `sum[0,63]` | 1 visit |

## Reading the output

Each transcript line reports one operation, what both structures answered, and
what each one paid:

```text
op05  min [16,23] = 2  ref 2  agree yes  | tree visits=7 pushes=2  | ref touches=8
```

- **`= 2` / `ref 2` / `agree yes`** — the lazy tree's answer, the brute-force
  array's answer, and whether they match. *This is the correctness proof.* The
  tree's answers are never checked against hand-computed constants; they are
  checked against a plain array doing the obvious thing, operation by
  operation. Drop a tag, apply one twice, or push one to the wrong child, and
  some query's two answers stop agreeing.
- **`visits`** — nodes the tree's descent entered (one unit per node).
- **`pushes`** — tags the descent had to push down to get its answer.
- **`touches`** — elements the reference array read or wrote (one unit per
  cell). A range-add over 32 cells really does write 32 cells.

Op 5 above is worth reading twice: it pays `pushes=2`, and those two pushes are
the deferred work left behind by op 3's `add[0,31]`, collected the moment a
query first needed to look below node 1.

The transcript is honest about where the tree *loses*. Op 5 costs 7 visits
against 8 touches, and op 11 (`set[20]`) costs 7 visits against a single
touch — a point write is trivially cheap for a plain array. The tree wins on
wide ranges, which is exactly the workload it exists for, and the script says
so in both directions.

The run then prints the totals, the deferred work still parked in the tree, and
the drain that finally settles it:

```text
work
  tree node visits: 68
  tree tag pushes: 14
  reference element touches: 825
  reference touches per tree visit: 12

deferred work still parked in the tree
  nodes holding a pending add: 13
  first such node, heap index: 0
```

Heap index 0 is **the root**: when the script ends, the root itself is still
holding op 17's `-2`, un-pushed. Thirteen nodes are sitting on adds that were
never propagated, because nothing ever needed to look below them. That is
lazy propagation, in the state it leaves behind.

Finally the tree is *drained* — every remaining tag pushed all the way to the
leaves — which takes 63 pushes (one per internal node: the root's tag cascades
to every one of them) and leaves 0 nodes pending. The settled leaves are then
compared cell by cell with the reference array, and the two dumps are printed
so the agreement is visible, not just asserted.

## Running

```sh
sh ci/checks/orchestrators/castle-tests.sh -- segment_tree_lazy
```

What this adds to the corpus: the first **deferred-update** data structure —
every other castle's writes land where they are written, while this one records
a range update as a lazy tag on whichever node covers it and pushes that tag
down only when a later operation is forced to descend through it, so work the
program has already promised is still sitting undone in the tree when the
script ends (13 nodes, the root among them). It pairs that structure with a
brute-force array replaying the same eighteen operations, and holds the two to
a hard contract in both directions at once: all twelve query answers agree and
the settled leaves match cell for cell, while the work counts must not — they
differ by an order of magnitude (825 element touches
against 68 node visits, 12:1). Agreement alone would be satisfiable by a
structure that quietly did the brute-force thing; the divergence in the
counters is what proves the laziness is real, and running both in lockstep is
what proves it is correct.

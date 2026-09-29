# skyline_silhouette

This castle computes the outline of a city — the classic sweep-line skyline
problem — and then checks its own answer by computing the same city's area a
second, unrelated way.

Seven buildings stand on a line. Each is a rectangle: a half-open span
`[left, right)` and a height. The program prints the buildings, the edge events
in the order the sweep visits them, the sweep itself, the resulting skyline as a
list of key points, and the area — twice.

There is no stdin fixture. The buildings are written into `skyline/city`.

## The buildings, and what each one is there to break

```text
h
12 |                                              T
11 | S S S S
10 | S S S S
 9 | S S S S
 8 | S S S S
 7 | S S S S                          D D D V T V V V
 6 | S S S S A A A P P                D D D V T V V V
 5 | S S S S A A A P P                D D D V T V V V
 4 | S S S S A A A P P                D D D V T V V V
 3 | S S S S A A A P P                D D D V T V V V
 2 | S S S S A A A P P                D D D V T V V V
 1 | S S S S A A A P P                D D D V T V V V
   +-------------------------------------------------
col  1 2 3 4 5 6 7 8 9 . . 12 13 14 15 16 17 18
```

| building | span | height | the degenerate case it plants |
| --- | --- | --- | --- |
| `spire` | `[ 1, 5)` | 11 | the tall one everything else is measured against |
| `annex` | `[ 2, 7)` | 6 | overlaps `spire` and outlives it to the right |
| `kiosk` | `[ 3, 4)` | 4 | **fully contained** inside `spire`, and shorter — it must contribute *nothing* |
| `porch` | `[ 7, 9)` | 6 | **equal-height touch**: begins exactly where `annex` ends, at exactly its height |
| `depot` | `[12,17)` | 7 | opens the second cluster, after a **gap** |
| `vault` | `[14,19)` | 7 | **equal height, overlapping** `depot` — the multiset must hold 7 *twice* |
| `tower` | `[15,16)` | 12 | a spike inside the overlap, poking above both |

Spans are half-open, which is what makes the touch expressible: `annex` ends at
7, so it occupies no ground at 7, and `porch` may begin exactly there without
the two ever overlapping. The silhouette must run flat at height 6 straight
through x=7 — no dip, no spurious key point. Between x=9 and x=12 nothing stands
at all, so the silhouette must fall to 0 and stay there. A naive implementation
gets at least one of these wrong.

## The sweep and the active multiset

Every building contributes two events: a **left edge**, which admits its height,
and a **right edge**, which withdraws it. The sweep visits the events from left
to right carrying the set of heights it is currently standing under, and emits a
**key point** exactly when an event *changes* the tallest of them. Events that
leave the tallest height where it was change nothing a viewer could see, however
much they change what is underneath.

Two parts of that carry the degenerate cases.

**The order of the events** (`skyline/events`, `event_leq`). Events sort by
position. At one position, every left edge is taken *before* every right edge —
that is what stops the skyline from dipping through the edge `annex` and `porch`
share, because the arriving 6 is already in hand before the departing 6 is
withdrawn. Two further tie-breaks stop one position from emitting two key
points: among left edges the taller goes first, and among right edges the
shorter goes first.

**The active heights are a multiset, not a set** (`skyline/multiset`). The host
offers no priority queue, so they are kept as a list sorted from tallest to
shortest — the current silhouette height is then just the head. Two buildings of
the same height are *two entries*, and a right edge withdraws exactly one of
them. That is what `depot` and `vault` are for: when `depot` ends at 17,
`vault`'s 7 has to survive. A set would collapse the two 7s into one, and
`vault`'s roof would vanish the moment `depot`'s ended.

## How to read the trace

The `events` block lists the edges in sweep order. The `sweep` block then shows,
for each one:

```text
   7  enter  top= 6  ---  [6,6]
   7  leave  top= 6  ---  [6]
  17  leave  top= 7  ---  [7]
```

— the position, which edge fired, the resulting silhouette height (`top=`),
whether a key point was emitted (`key`) or not (`---`), and the active multiset
after the event, tallest first. The two rows at position 7 are the equal-height
touch: the multiset briefly holds *both* 6s, one is withdrawn, `top` never
moves, and no key point is emitted. The row at 17 is the multiset again: `depot`
leaves, `vault`'s 7 remains, and the silhouette does not notice.

## The area, computed twice

`skyline/area` computes the silhouette's area from the key points: between two
consecutive points the outline is flat, so each pair contributes
`height x width`. It then computes the same area *without the sweep at all* —
rasterizing every building onto a per-unit-column array (`Array[I32]`), each
column holding the tallest building standing on it, and adding the columns up.
Nothing the sweep touches is reused. The two numbers must agree, and stdout says
whether they do.

They catch different mistakes, so the program prints both verdicts:

- **`areas agree`** catches a broken multiset. Treat the active heights as a set
  and `vault`'s roof disappears at x=17; the key points then claim an area of 96
  where the raster still says 122.
- **`points advance`** — key points strictly increase in x — catches a broken
  event order. Visit right edges before left edges and the shared edge at x=7
  emits a spurious `(7,0) (7,6)` pair. The areas *cannot* see this: a pair at one
  position spans no width, so it costs no area, and both totals still say 122.
  Strict advance is what pins it.

What this adds to the corpus: the corpus's first computational geometry — a
sweep-line skyline over an active height multiset, with contained buildings,
equal-height touches and gaps as the degenerate cases, and an independent
rasterized area that must agree with the area computed from the key points. It
composes a polymorphic cons list (events, active heights, key points) against a
mutable host `Array[I32]` (the raster) in one program, derives its whole
comparison vocabulary — equality, strict order, maximum — from the single
`leq_i32` the host offers, and uses a module-local `op _ ++ __` concatenation
chain to build the transcript.

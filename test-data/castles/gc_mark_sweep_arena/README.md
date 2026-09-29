# gc_mark_sweep_arena

A mark-sweep-compact garbage collector over a simulated heap. The
program builds a fixture arena in Kio, traces it from a root set with a
worklist marker, sweeps it into a census of live and dead objects,
slides the survivors down over the garbage through a forwarding table,
and then verifies the compacted heap from scratch. It reads no input;
the fixture is source.

## The heap

The arena is one host `Array(I32)` of cells. Objects are packed
contiguously from address 0 with no gaps, each laid out as

```text
[tag] [field count] [field 0] ... [field n-1]
```

so an object occupies `2 + n` cells and the object list needs no side
table: stepping over an object's size lands exactly on the next header.
The arena's length *is* its allocation top, so the collector reclaims
cells by popping them off the end rather than tracking a separate bump
pointer.

A field cell holds either an immediate or a pointer to an object header.
The two are told apart by a bias: a cell at or above `1000` is a pointer
to `cell - 1000`, and any smaller cell is an immediate. The bias sits
far above the arena's top, so the ranges cannot overlap, and decoding a
field costs one subtraction -- no division or bit operations, neither of
which the host env supplies. Immediates are therefore non-negative and
below the bias. Four tags name the object kinds: `frame`, `pair`, `vec`
and `box`.

The root set is a small array of addresses -- the collector's only
starting points, and the only thing it rewrites besides the arena.

## The fixture object graph

Nine objects, 36 cells, two roots (`active` and `temp`). The graph is
built the way a linker builds an image: every object is allocated in
address order with its pointer fields left as holes, and the pointers
are patched once every address is known, so no address is hard-coded.

| object | tag | address | fields |
| --- | --- | --- | --- |
| `active` | frame | `@0` | `*left`, `*right`, `7` |
| `dropped` | frame | `@5` | `*stale`, `99` |
| `left` | pair | `@9` | `*shared`, `*back` |
| `right` | pair | `@13` | `*shared`, `5` |
| `stale` | pair | `@17` | `*dropped`, `*orphan` |
| `shared` | vec | `@21` | `3`, `4`, `5` |
| `back` | box | `@26` | `*left` |
| `orphan` | vec | `@29` | `11`, `22` |
| `temp` | box | `@33` | `*shared` |

It is deliberately awkward for a collector:

- **A live cycle.** `left` and `back` point at each other, so a tracer
  that recursed into the heap would not terminate.
- **An unreachable island.** `dropped`, `stale` and `orphan` are
  garbage, and `dropped` and `stale` point at each other -- a cycle, so
  reference counting alone would never free them.
- **A shared object.** `shared` is reached three ways: through `left`,
  through `right`, and straight from the second root. The tracer meets
  it more than once and must trace it only once.
- **Immediates-only objects.** `shared` and `orphan` carry no pointers.
- **Interleaved garbage.** The dead objects sit at the bottom, the
  middle and near the top of the arena, so compaction has to move every
  survivor except the first.

## The phases

**Mark** seeds an explicit `Array(I32)` stack with the roots and drains
it: each pop either marks a fresh object and pushes its pointer fields,
or drops an object already marked. Nothing recurses into the heap, so
the cycle costs one extra pop instead of unbounded stack depth. The
worklist is drained through `array_pop_back`, whose `I32 | .` result
reports the empty stack as its unit arm.

**Sweep** walks the object list once and turns the mark bits into a
census -- live and dead objects, live and dead cells. A sliding collector
has no free list to thread, since the free space ends up as one run at
the top of the arena.

**Compact** runs three passes. `plan` records each survivor's new
address in a forwarding table indexed by its *old* address. `slide`
copies each survivor's cells down; every object moves down or stays put
while the walk goes up, so a copy only ever writes below the header it
is about to read next and needs no scratch arena. `relocate` then
rewrites the roots and every pointer field through the table -- after the
move, pointers still hold old addresses, which the copy carried along as
data. Finally the arena is popped down to the new top, which is what
actually reclaims the cells.

**Verify** takes none of that on trust. It rebuilds the header map from
the compacted arena and checks that every root and every pointer field
lands on a cell that *starts* an object -- being in range is not enough,
since a pointer into the middle of an object is exactly what a botched
relocation produces. Only if every pointer is sound does it re-walk
reachability from the relocated roots with a fresh worklist and compare
the count against the live count the sweep took before anything moved.
The order is deliberate: tracing is what follows pointers, so tracing an
unvalidated heap would turn a diagnosable fault into a crash.

## Output shape

The program owns all of its output; it reads no input.

```text
roots: <root addresses>

heap before gc (<cells> cells, <objects> objects)
@<addr> <tag> n=<field count> [<fields>]      one line per object,
...                                           a pointer rendered as *<addr>

-- mark --
worklist pushes / pops / revisits skipped / marked objects

-- sweep --
live objects: <n> (<addresses>)
dead objects: <n> (<addresses>)
live cells / dead cells

-- compact --
forwarding table (old -> new)
  @<old> -> @<new>                            survivors only; a dead
...                                           object leaves no entry
roots after fixup: <relocated roots>
arena top: <before> -> <after>
cells reclaimed: <n>

heap after gc (<cells> cells, <objects> objects)
...                                           same rendering as before gc

-- verify --
objects in arena / roots checked / pointer fields checked
dangling pointers: <n>
reachable from roots: <n> of <n>
verdict: heap verified
```

The two heap maps use the same rendering, so reading them side by side
is enough to check the collector by hand: every surviving object appears
in both, moved down and with its pointers rewritten.

The mark counters are the traversal's own audit. `pushes` must equal the
number of roots plus the number of pointer fields in live objects, and
`pops` must equal `marked + revisits` -- so the three revisits are the
cycle and the shared object being met a second time and turned away.

## What this adds to the corpus

The first memory-manager castle: a garbage collector, with worklist
marking over an explicit `Array(I32)` stack (drained through
`array_pop_back`'s `I32 | .` sum), a sweep census, sliding compaction
through a forwarding table with root and pointer-field fixup, and a
post-compaction reachability verification that re-derives the heap's
soundness from scratch. It is the corpus's first program to combine the
host-array surface with the `elab` elaborator library -- `match!`
dispatches the tagged field slots and the worklist's empty-stack arm,
and `widen_sum!` builds them -- and its object-list and field-list walks
go through two polymorphic `rec(loop)` folds that take a step closure,
so every phase supplies only what it does at each object rather than
re-hand-rolling the traversal.

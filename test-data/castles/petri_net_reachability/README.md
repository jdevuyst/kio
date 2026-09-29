# Petri-net reachability

This program explores a bounded Petri net for two jobs that need two locks.
The left job acquires lock A first; the right job acquires lock B first.
Each finishes by acquiring the other lock and then releasing both. Tokens
record waiting, holding, and finished jobs, plus the two available locks.
Every place has capacity one.

The fixture lives in `workdir/petri/fixture.kio`; the program reads no stdin.
Transition order is acquire-left, acquire-right, finish-left, finish-right.
A FIFO breadth-first search explores every reachable marking, compares token
vectors structurally, and records only the first predecessor of each state.
It distinguishes a completed terminal marking from an unfinished deadlock.
Both orders of acquisition converge on circular wait, and both successful
job orders converge on the same completed marking.

Stdout reports graph counts, the first deadlock's shortest witness, and each
replayed transition. Replay starts from the fixture again, checks every
transition, and compares its final marking with the discovered deadlock.
Additional successful checks reject finishing before acquisition and a
capacity-overflow transition, and distinguish an initially completed net
from an initially stuck one.

The Queue package supplies the actual search frontier. Its bundled List
supplies token vectors, arcs, visited nodes, and witness steps. Both library
interfaces use local adapters to the package's arithmetic and loop bindings.

What this adds to the corpus: exhaustive Petri-net graph search with a live
persistent FIFO, structural duplicate detection, predecessor reconstruction,
and independent witness replay. Converging paths exercise discovery-time
deduplication while terminal classification separates success from deadlock.

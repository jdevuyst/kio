# maze_replay

A seeded grid-walk replay. The program reads scenario settings and a
fixture seed from stdin, turns the seed into a deterministic
obstacle-and-item field with a Kio-implemented PRNG, replays a sequence
of cardinal commands against the generated grid, and prints a compact
transcript summary.

## What it models

- A small linear-congruential generator (`maze/prng`) seeded from the
  fixture. The modulus, multiplier, and increment are chosen so every
  step is overflow-free in signed 32-bit arithmetic.
- A procedurally generated world (`maze/world`): each cell's kind —
  wall, item, or open — is a pure function of the seed and the cell's
  coordinates, so nothing is stored and the field is fully
  reproducible. The start corner `(0, 0)` and the goal corner
  `(w-1, h-1)` are always open.
- A player replay (`maze/replay`): the player starts at the origin and
  applies one command per step. A recognized command (`N` / `E` / `S` /
  `W`) into an in-bounds, non-wall cell is accepted — the player enters
  it, banking an item when the cell carries one. A move off the grid or
  into a wall is a blocked bump that leaves the position unchanged. An
  unrecognized command is ignored.

## Input shape

`input.stdin`, one value per line:

1. the PRNG seed,
2. the grid width,
3. the grid height,
4. the number of replay commands,
5. then that many cardinal commands, one per line.

The checked-in fixture uses seed `7` on a `5x4` grid with ten commands:
two leading off-grid bumps (`N`, `W` from the origin), the escaping
path `S S S E E E E`, and a trailing unrecognized `X`. That single run
exercises accepted moves, item collection, out-of-bounds bumps, an
ignored command, and reaching the goal.

## Output shape

A summary in domain terms — the program owns all output; the runner
echoes nothing:

```text
maze_replay: v1
seed: <seed read from the fixture>
grid: <w>x<h>
goal: <gx>,<gy>
commands: <count>
checksum: <seed/grid-derived fingerprint>
final: <x>,<y>
moves: <accepted moves>
bumps: <blocked moves>
score: <items collected>
status: escaped | stranded
```

The `checksum` is the running sum (mod the PRNG modulus) of every
cell's mixed value across the whole grid — a fingerprint that changes
with the seed or the dimensions, pinning the generated world.

## What this adds to the corpus

The first castle. It is the corpus's first end-to-end exercise of
line-driven fixture input through `read_ascii_line()`, fixture parsing
in Kio via `string_to_int`, a fixture-seeded PRNG implemented in Kio,
`rec(loop)` recursion whose recursive call sits in a `match!` clause
body, and a `labels` sum declared and matched inside a submodule —
composed into one coherent program rather than a minimized regression
case.

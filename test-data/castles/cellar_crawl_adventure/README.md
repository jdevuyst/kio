# cellar_crawl_adventure

A small text adventure. Seven rooms under an old vineyard are wired into a
directed graph whose edges are gated by what the player is carrying: two doors
open only to the brass key, and one room is unlit and turns away anyone without
the lantern. The player drives it from stdin with a seven-verb command
language, and the program prints the whole transcript — the runner echoes
nothing.

## The map

Rooms are nodes; a `(room, direction)` pair picks an edge. The graph is a total
function whose branches spell out each hand-authored edge and whose fallback
represents an absent edge.

```text
                     Foot of the Cellar Stairs   (start)
                                | north
                                v
        Cooperage  <--- west  Barrel Hall  east --->  Tasting Nook
        (brass key)              |                    (cork)
                                 | down  [LOCKED: brass key]
                                 v
                          Racking Floor
                          (lantern)
                                 | north
                                 v
                          Unlit Cellar  [DARK: needs the lantern]
                                 |
                                 | east  [LOCKED: brass key]
                                 v
                          Reserve Vault
                          (dusty bottle -- the prize)
```

Every edge is two-way except the gates themselves, which are one-way facts
about the edge, not the room: the trapdoor down from the Barrel Hall and the
vault door east out of the Unlit Cellar are both locked, and both answer to the
same brass key — a cellar master carries one key, not two. Unlocking is a state
change, so the exits line reports a door as `down (locked)` before it is opened
and plain `down` afterwards.

**Darkness** gates two things and not a third. Walking *into* an unlit room
without a light is refused, and `look` inside one shows nothing. Walking *out*
is always allowed — the player feels their way back — and so is `take`, which
lets a player recover a lantern dropped in the Unlit Cellar. The fixture walks
straight into that corner on purpose.

## Items

| item | starts in | worth |
| --- | --- | --- |
| lantern | Racking Floor | 0 (a tool) |
| brass key | Cooperage | 0 (a tool) |
| cork | Tasting Nook | 5 |
| dusty bottle | Reserve Vault | 50 (the prize) |

An item is always *somewhere* — lying in a room, or held by the player — so the
inventory is derived from item locations rather than stored a second time and
kept in step.

## The command language, and why arguments get their own line

Seven verbs:

```text
go <direction>      take <item>      drop <item>      unlock <direction>
look                inventory        quit
```

Directions are `north`, `south`, `east`, `west`, `up`, `down`. Item words are
`lantern`, `key`, `cork`, `bottle`.

**A verb and its argument arrive on two separate lines.** `go north` is the
line `go` followed by the line `north`. The `testapi-bare-compute` tier supplies
whole-string equality but no string slicing or search, so parsing is two-stage:
classify the verb, then read an object line only when that verb takes one. A
mistyped verb therefore consumes one line rather than swallowing the command
after it (`dance` consumes one line, not two).

Reading fixture input is `read_ascii_line()` (`String | .`, `()` at EOF), and
dispatch is `match!` over the command sum.

## Failures are values

Every way the cellar can say no is an arm of one `Outcome` sum — a locked way, a
dark doorway, a word that is not a direction, an item that is not here, a verb
that does not exist — so a refused command is a value the engine returns, never
a crash. `cellar/engine` mints outcomes and never prints; `cellar/report` is the
only module that turns one into English. `match!` makes the "is this turn a
fumble?" classification exhaustive, so the score cannot silently miss a case.

The fixture exercises **all seventeen** outcome arms.

## Integer equality without an integer comparison

The `testapi-bare-compute` tier supplies `add` / `sub` / `mul` / `div` / `mod`
and **no integer comparison at all** — no `eq_i32`, no `lt_i32`, no `leq_i32`.
Room, direction and item identities are numbers, so the program needs equality
and has to build it:

```text
int_eq(a, b)  =  string_eq(int_to_string(a), int_to_string(b))
```

Decimal rendering is injective on `I32` — one value has exactly one spelling, no
leading zeros and no negative zero — so equal spellings mean equal values. That
one function in `cellar/ids` is the whole of the program's integer comparison;
every `room_eq` / `dir_eq` / `item_eq` goes through it. Nothing else needs an
ordering.

The tier also has no `bool_to_string`, so every Bool the transcript shows is
rendered by a hand-written `if` / `else` (`won` / `left behind`).

## Scoring

```text
score = 10 * doors opened  +  value of what you are carrying  -  1 per fumble
```

Score is a pure function of the final state, not a running tally, so it cannot
double-count an item that was dropped and picked up again. A rank follows from
`div(score, 25)`: Cellar Rat, Cork Sniffer, Bottle Hunter, Cellar Master.

## Input

`input.stdin` is 68 lines of plain ASCII: one verb per line, each object on the
line after its verb. It is a winning playthrough that walks the player from the
stairs to the bottle and back out, and it deliberately fumbles along the way. In
order, it covers:

- a move with no exit that way (`go up` from the stairs) and a word that is not
  a direction (`go sideways`);
- a **blocked move** — `go down` refused by the locked trapdoor — and then
  `unlock down` refused again, because the key is still in the Cooperage;
- fetching the key, taking something that is not there (`take lantern` in the
  Cooperage), and taking something already held;
- the cork, and `unlock north` where there is no lock at all;
- **unlocking the trapdoor with the key**, and going down;
- **entering the dark room without the lantern** — refused — then taking the
  lantern and entering with it;
- dropping the lantern *inside* the dark room, looking (pitch dark), and taking
  it back;
- unlocking the vault, **taking the prize**, an unknown noun (`take unicorn`),
  an **unknown verb** (`dance`), and dropping something never carried
  (`drop sword`);
- walking back out and `quit`.

The last line of the fixture is a `look` that is **never read**: `quit` ends the
loop, so a run that consumed it would show a 38th turn. The transcript stops at
37, which is what pins `quit`.

## Output

A transcript: each command echoed after a `> ` prompt, then what the cellar did
— a room description on arrival or `look`, the reason for a refusal, the item
taken. Then a final tally: turns, moves, fumbles, doors opened, inventory,
score with its arithmetic spelled out, rank, and whether the prize was won. The
checked-in run ends at 37 turns, 12 moves, 12 fumbles, both doors open, all four
items carried, and `score: 63 = 20 doors + 55 haul - 12 fumbles` — rank Bottle
Hunter, prize won.

What this adds to the corpus: the corpus's first interactive-fiction world model
— a room graph whose edges are gated by inventory state, a stdin command
interpreter whose failures are values, and the first castle on the
unsuffixed-arithmetic bare-compute tier, which supplies no integer comparison at
all.

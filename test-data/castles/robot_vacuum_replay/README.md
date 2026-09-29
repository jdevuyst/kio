# robot_vacuum_replay

`robot_vacuum_replay` replays a robot vacuum command log from stdin. Each
input line is one ASCII command. Whole-line text commands are matched with the
canonical string helpers, and a line that parses as an integer is treated as a
battery charge adjustment through `string_to_int`.

The command vocabulary is:

- `START` enters cleaning mode.
- `PAUSE` enters paused mode.
- `LEFT` and `RIGHT` rotate the robot.
- `STEP` moves one tile while cleaning; otherwise it records a fault.
- `CLEAN` removes one dirt counter while cleaning; otherwise it records a fault.
- `DOCK` returns to the dock at `(0,0)`, points north, and recharges.
- An integer line adjusts battery directly.
- Any other line records an unknown-command fault.

Stdout starts with a header, then prints one compact transcript line per input
line, followed by a final summary. Each transcript line shows the normalized
command, the current mode, position, facing direction, battery, dirt count, and
fault count after applying that command.

What this adds to the corpus: this is a stdin-driven replay state machine with
labeled state, labeled command sums, imported `match!` dispatch from the `elab`
POC, `rec(loop)` EOF handling, whole-line text parsing, integer parsing, and
the `testapi-compute` host surface across all executable backends.

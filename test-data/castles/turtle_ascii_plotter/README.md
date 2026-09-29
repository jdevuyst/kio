# turtle_ascii_plotter

A turtle-graphics rasterizer. The program reads a turtle program from stdin,
parses it into a command tree with nested `repeat` blocks, runs that tree, and
prints the picture the turtle drew as an ASCII canvas.

It is a two-phase interpreter, and deliberately so. `turtle/parse` reads the
whole source into a tree before anything executes, so a `repeat` carries its own
body and the interpreter in `turtle/exec` never has to scan forward for a
matching `end`. The echoed program in the output is what makes that visible: it
is printed from the parsed tree, indented, so a `repeat` that swallowed the
wrong commands would show up before the picture does.

## The command language

**One token per line.** That is the whole convention. A keyword sits on its own
line, and each argument it takes sits on the line after it. There is no
whitespace splitting anywhere in the program, because this host tier has no
string slicing and no character access — a line is only ever compared whole
(`string_eq`) or parsed as a number (`string_to_int`). So `move 4` is spelled

```text
move
4
```

and `pen down` is spelled `pen` on one line, `down` on the next.

The first three lines are the header: the word `canvas`, then the width, then
the height. They are consumed before parsing begins and are not part of the
program.

| Keyword | Argument lines | Meaning |
| --- | --- | --- |
| `pen` | `down` or `up` | Lower or raise the pen. Lowering it inks the cell the turtle is standing on. |
| `move` | `<n>` | Advance `n` cells along the current heading, inking each cell entered while the pen is down. |
| `turn` | `left` or `right` | Rotate 90 degrees. |
| `goto` | `<x>`, `<y>` | Teleport. It draws no line between the old cell and the new one, but the pen still marks where it lands. |
| `mark` | `<code>` | Stamp the current cell with glyph `<code>`, whatever the pen is doing. |
| `repeat` | `<n>` | Run the commands up to the matching `end` `n` times. `repeat` blocks nest. |
| `end` | — | Close the innermost open `repeat`. |

The turtle starts at `(0, 0)` facing east with the pen up. `x` is the column and
`y` is the row, and row `0` prints first — so `north` walks toward the top of
the picture.

### Ink, and what gets clipped

The turtle walks an **unbounded** integer plane; the canvas is only the window
onto it. Motion is never blocked. What is clipped is *ink*: an ink operation
whose target cell lies outside the canvas is discarded, and the clip counter
rises. One rule covers every case — a stroke running past an edge, a `mark`
outside the frame, a pen lowered off-canvas. A stroke may therefore run off the
paper, keep going, and pick the drawing back up where it walks back on, which is
exactly what the fixture makes it do.

### Glyphs

`mark` names a glyph by number, since the program has no way to write a
character it could index. Code `0` is the eraser.

| Code | 0 | 1 | 2 | 3 | 4 | 5 | anything else |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Glyph | `.` (blank) | `*` | `#` | `+` | `o` | `@` | `?` |

The pen never chooses: it always draws with `*`. Code `1` and the pen's stroke
are the same ink.

### Malformed input

The parser is total, and says so out loud rather than dropping anything. An
unknown keyword, an unknown `pen`/`turn` word, and an `end` with no `repeat`
open all become `unknown` commands carrying the offending word, which the echo
prints. A numeric argument that will not parse reads as `0`, and end of input
closes whatever blocks are still open.

## Input shape

`input.stdin` holds the fixture program. Read as commands rather than lines, it
is:

```text
canvas 20 10        # header: a 20x10 canvas
goto 5 1            # move to the first box corner (pen still up)
pen down            # inks (5,1)
repeat 3            # three boxes in a row
  repeat 4          #   one box: four sides
    move 3
    turn right
  end
  pen up            #   hop to the next corner without drawing
  move 5
  pen down          #   the third hop lands at x=20 -- off the paper, clipped
end
pen up
goto 16 6           # a stroke that runs off the east edge...
pen down
move 6
turn right
move 2              # ...wanders outside the canvas entirely...
turn right
move 6              # ...and comes back on to finish the row-8 stroke
pen up
goto 5 1
mark 5              # '@' over the box corner
goto 7 1
mark 0              # erase one cell of the top edge
goto 2 8
mark 9              # an unknown glyph code, so '?'
goto 25 5
mark 3              # off the paper: clipped
spin                # not a keyword: reported as `unknown spin`
turn left
goto 1 7
pen down
move 4              # runs off the bottom edge: two steps clipped
```

One run exercises nested bounded loops, a `repeat` count driving a shape, ink
clipped at three different edges, a stroke that resumes after leaving the
canvas, an erasing `mark`, an out-of-alphabet glyph, an out-of-canvas `mark`,
and an unrecognized command.

## Output shape

Three sections, in order. The runner echoes nothing — the program prints all of
it.

1. **`program:`** — the parsed tree, printed back with two spaces of indent per
   nesting level and an explicit `end`. This is the proof the nesting parsed:
   the inner `repeat 4` is indented under the outer `repeat 3`.
2. **`plot:`** — the canvas, one line per row, row `0` first, exactly `width`
   glyphs per line. Blank cells are `.`, so every row is full width and the
   picture needs no border.
3. **`stats:`** — `commands` is the number of primitive commands executed after
   `repeat` expansion (`repeat` and `end` are control structure and are not
   counted, but an `unknown` command is — it executes as a no-op). `inked` is
   the number of cells carrying ink at the end. `clipped` is the number of ink
   operations discarded for landing off the paper. `final` is where the turtle
   stopped, which for this fixture is *outside* the canvas — the last `move`
   walked it off the bottom edge and nothing stopped it.

`inked` is worth checking by eye: it is exactly the count of non-`.` characters
in the plot printed directly above it. The fixture's three boxes contribute
their perimeters, one cell of the first box's top edge is erased by `mark 0`,
and the two clipped strokes contribute only the parts that landed on the paper.

What this adds to the corpus: the first rasterizer — the first castle whose
observable output is a *picture* rather than a transcript or a summary. It is
the corpus's first two-phase parse-then-execute interpreter, where a recursive
descent builds a command tree with nested bounded loops (`repeat` inside
`repeat`) and a separate tree-walking evaluator runs it, so the parse is
checkable in the output independently of the execution. It is the first
line-rasterization castle: strokes are drawn one cell per step with edge
clipping, on a turtle that keeps walking off an unbounded plane while its ink is
discarded, and resumes drawing when it returns. And it is the first castle to
build a two-dimensional grid as a pure Kio value — a list of rows of cells, with
every cell write rebuilding one row — because the tier offers no host array to
hide the rasterization in. Both structures use the public generic `list`
package through a materialized path dependency: the recursive command tree
carries each `repeat` body as `list.List(Command)`, while the canvas instantiates
the same collection at rows and ink codes. The castle therefore composes one
adopted collection across recursive syntax and persistent raster data instead
of carrying a castle-local list representation.

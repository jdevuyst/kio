# tape_macro_interpreter

This castle is a small tape macro interpreter with its bytecode fixture compiled
into the Kio source. The program walks a three-cell tape with a pointer, applies
add/subtract and move commands, emits checkpoints, and expands one macro into a
short sequence of virtual commands before resuming the outer program.

There is no `input.stdin`. The selected runner protocol is
`testapi-arith-collection`, which supplies printing, numeric formatting, string
concatenation, i32 arithmetic/comparison, booleans, and the host `loop`
capability without any stdin declaration.

Stdout is the interpreter transcript: a program heading, checkpoint lines that
show the instruction pointer, active tape pointer, tape cells, and checksum, and
a final summary with the completed state.

What this adds to the corpus: this is a no-stdin parser/interpreter-shaped
castle using `rec(loop)` for a stateful VM walk, label-shaped interpreter state,
host arithmetic/text formatting, and a fixed macro expansion path. It stresses
backend emission for recursive state threading, numeric dispatch, and side-effect
sequencing without depending on stdin, arrays, or POC libraries.

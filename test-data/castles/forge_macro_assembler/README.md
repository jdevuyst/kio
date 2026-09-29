# forge_macro_assembler

A macro assembler and virtual machine for FORGE-4, a small register
machine. Nine programs, written as in-source fixtures, each go through the
whole toolchain - macro expansion, two-pass assembly, then execution on a
fuel-bounded machine - and each is reported with its disassembly, its
output, and what it cost.

There is no `input.stdin`. The fixtures *are* the input; the runner
protocol is `testapi-compute` and the program reads nothing.

## The FORGE-4 machine

Four general registers (`a`, `b`, `c`, `d`), an operand stack, and a
program counter. Arithmetic is two-address: an opcode reads both operands
and writes the destination register.

| Opcode | Effect |
| --- | --- |
| `LOADI r, n` | `r <- n` |
| `MOV d, s` | `d <- s` |
| `ADD` / `SUB` / `MUL` / `DIV` / `MOD` `d, s` | `d <- d op s` |
| `JMP label` | jump |
| `JMPZ r, label` | jump when `r` is zero |
| `PUSH r` / `POP r` | operand stack |
| `EMIT r` | append `r` to the output log |
| `HALT` | stop |

Nothing traps into the host. A zero divisor and a pop on an empty stack
stop the machine with a fault; a program counter that walks past the last
instruction simply runs out of instructions; and every fetched instruction
spends one unit of a step budget carried in the machine state, so a program
with a runaway loop stops with `out of fuel` rather than hanging. Those
four stop reasons are the machine's whole vocabulary, and the fixtures
reach all four.

## The pipeline

A source program is a list of items: an instruction, a label mark, or a
macro call. Three phases consume it, and each phase's type says what the
one before it removed.

1. **Macro expansion** (`forge/expand`) replaces every call with the body
   bound to its name. A body is straight-line code - no marks, no nested
   calls - so one walk suffices. Macros are deliberately unhygienic: a body
   may jump to a label the *caller* defines, which is how `drain_tail`
   closes the `series` program's drain loop.
2. **Pass one** (`forge/assemble`) gives each instruction the next address
   and binds each mark to the address of the instruction that follows it,
   producing straight-line code plus a symbol table.
3. **Pass two** resolves every symbolic jump to a numeric target.

Source and resolved instructions are two different types: a source `jmp`
names a label, a resolved one carries an address. The eleven opcodes that
never mention a jump target are declared once in `forge/isa` and *reused*
by the second declaration, so a `LOADI` read out of a source program is
already an arm of the resolved set and crosses pass two untouched. Only the
two jumps are rebuilt.

Assembly is fallible in exactly three ways - an unknown macro, an undefined
label, a label marked twice - and each phase's return type admits only the
failure that phase can raise.

## The programs

Six assemble and run:

- **gcd** - Euclid on 252 and 105. Emits both inputs, then each remainder,
  then the gcd, then 252 divided by it. `MOD` runs three times with a live
  divisor and `DIV` once; `JMPZ` both falls through and takes its branch.
- **series** - the squares of 5 down to 1. One loop stacks each square and
  sums them; a second drains the stack, emitting the squares in the order
  they come back off, with the total last. All three macros expand here.
- **spin** - a jump to itself. The fuel budget is what stops it.
- **runoff** - no `HALT`; it walks off the end. It also calls `dec_a`, the
  macro `series` uses, which is what makes that macro a shared template
  rather than a one-off.
- **div_zero** - a zero divisor. `DIV` and `MOD` share one guard, so this
  is the fault both of them raise.
- **pop_empty** - one push, two pops.

Three are rejected, one per way assembly can fail: **no_macro** calls a
macro the table does not define, **no_label** jumps to a label nothing
marks, and **twice** marks one label twice.

The suite is sized so that every opcode, both jump forms, every macro,
every stop reason, and every rejection is reached by some program. Nothing
in the source is left unexercised.

## Output

One section per program, readable without the source.

```text
== gcd ==
items: 18   expanded: 18   instructions: 16
symbols: loop=0004 done=0011
listing:
  0000  LOADI  a, 252
  ...
  loop:
  0004  JMPZ   b, 0011
  ...
emit: 252 105 42 21 0 21 12
registers: a=21 b=0 c=0 d=12
stop: halted
steps: 31/120
stack: final 0, max 0
```

`items` is the size of the source program, `expanded` its size after macro
expansion (the two differ exactly when macros fired), and `instructions`
the size of the assembled image. `symbols` is the label table pass one
built. `listing` is the disassembly: every instruction at its address, each
label on its own line above the address it names, and every jump now
numeric. `emit` is what the program's `EMIT` instructions logged, in order.
Then the registers it left behind, why it stopped, the steps it spent
against its budget, and the stack's final and high-water depth. A rejected
program shows its size and the reason instead.

A closing line tallies the run.

## Notes on the source

`forge/list` is the one collection: a persistent cons list, used at seven
instantiations (items, instructions, symbols, macros, machine words, and
the two intermediate forms). The VM's instruction fetch is `drop` then one
`un_cons`, so a program counter past the end finds nothing and ends the run
without a separate bounds check.

Kio ships no list literal, so the fixtures in `forge/programs` splice items
onto an accumulator with UFCS, which makes a program read down the page the
way an assembly listing does; `forge/macros`, whose bodies are one to three
instructions, uses a `fold` bracket binding instead.

The host tier supplies no `eq_i32`, so integer equality is derived from the
antisymmetry of `<=`, and no string-length primitive, so the listing's
mnemonic column is padded in the literal rather than measured.

What this adds to the corpus: the first multi-phase assembler/compiler
pipeline - a dedicated macro-expansion pass, two-pass label resolution, a
fuel-bounded VM, and a disassembler that renders the resolved image back to
a listing - where `tape_macro_interpreter` expands one macro inline while
interpreting and `river_bytecode_vm` runs a fixed instruction table with no
assembly step at all. It is also the first compute-tier castle that reads no
stdin, the corpus's first use of `labels` reuse (`{ opcode: _ }`) to share
nominal types across two phases of one instruction set, and its first use of
a `fold` bracket binding as a list literal.

# cpu_pipeline_hazard_sim

A classic five-stage in-order CPU pipeline — IF, ID, EX, MEM, WB — running a
small register machine cycle by cycle, with a hazard unit and a forwarding
network. The program runs the **same code twice**: once with forwarding
disabled, so every read-after-write dependence has to be waited out, and once
with it enabled, so most of them are bypassed. It reads no input; the machine,
the program and the memory image are all source.

The headline is the cycle count: **33 cycles without forwarding, 22 with** —
and the register file and memory come out identical either way. That identity is
the point. Forwarding decides *when* a value is available, never *what* it is,
so a bypass network that changed a result would not be a faster pipeline; it
would be a broken one.

## The machine

Eight registers `r0`–`r7` and eight words of memory, every value an `I32`. `r0`
is an ordinary register — nothing is hardwired to zero — and it stays `0` only
because the fixture never writes it.

Each of the five stages holds at most one instruction. In the state record a
stage holds that instruction's index in the program, and a negative index is the
stage's valid bit turned off.

## The ISA

| instruction | meaning | reads | writes |
| --- | --- | --- | --- |
| `ADD rd, rs, rt` | `rd <- rs + rt` | `rs`, `rt` | `rd` |
| `SUB rd, rs, rt` | `rd <- rs - rt` | `rs`, `rt` | `rd` |
| `MUL rd, rs, rt` | `rd <- rs * rt` | `rs`, `rt` | `rd` |
| `LOADI rd, imm` | `rd <- imm` | — | `rd` |
| `LOAD rd, [addr]` | `rd <- mem[addr]` | — | `rd` |
| `STORE rs, [addr]` | `mem[addr] <- rs` | `rs` | — |
| `BEQ rs, rt, tgt` | branch to `tgt` when `rs == rt` | `rs`, `rt` | — |
| `NOP` | nothing | — | — |
| `HALT` | stop fetching | — | — |

The opcode is a label-generated sum and the instruction is one record with five
fields; the fields an opcode does not use are zero. Addresses are immediates, so
a `LOAD` reads no register and can never have a hazard on its own input — which
is exactly what makes it the *producer* in the interesting one.

A program must reach a `HALT`. `HALT` seals the front end the moment it is
**fetched**, and that is the only thing that stops the program counter; a program
without one runs off the end of the code array and faults there.

## The timing model

This is the part the whole castle turns on, so it is worth stating exactly.

- **The register file is written in the first half of a WB cycle and read in the
  second half of an ID cycle.** An instruction that reaches WB has therefore
  already landed, as far as the instruction in ID is concerned.
- **Operands are consumed in EX.** When the instruction now in ID reaches EX next
  cycle, the instruction now in EX will be in MEM — its result sitting in the
  **EX/MEM** latch — and the instruction now in MEM will be in WB, its result
  sitting in the **MEM/WB** latch.

Those two latches are the two bypass paths, and they are the reason the hazard
unit scans **EX and MEM and nothing else**: a producer any further along has
already written the file. (This is also why two paths are enough. Without the
split-cycle register file a dependence at distance three would be unreachable by
any bypass into EX, and forwarding could not claim to cover everything but the
load.)

So:

- **Forwarding off** — there are no latches to read from, so a consumer waits
  until its producer has left MEM: **two stall cycles** for a back-to-back
  dependence, one at distance two, none beyond that.
- **Forwarding on** — every RAW dependence is bypassed, *except one*. A `LOAD` in
  EX has not touched the memory port yet; its data arrives at the end of the
  **next** cycle, one cycle after the EX/MEM latch is read. No bypass carries a
  value backwards in time, so the consumer stalls for **exactly one cycle**,
  after which the load is in MEM and the ordinary MEM/WB path covers it. That is
  the **load-use hazard**, and it is the one forwarding cannot resolve.
- **A branch resolves in EX.** When it is taken, the two instructions already
  fetched behind it are wrong-path and are squashed — a two-cycle penalty. A
  branch compares two registers, so it is an ordinary consumer too: its operands
  stall or get bypassed like anything else.

A store reads its data register in EX and carries the value down in the EX/MEM
latch, so a store is a consumer as well. Loads and stores both touch memory in
MEM and nowhere else, and MEM is in order, so a store followed by a load of the
same word always sees the store first — there is no memory hazard to detect.

## The fixture program

Seventeen instructions, chosen so that every hazard class fires.

```text
i0   LOADI r1, 6           r1 = 6
i1   LOADI r2, 7           r2 = 7
i2   ADD   r3, r1, r2      r3 = 13    <- reads r2 at distance 1 and r1 at distance 2
i3   MUL   r4, r3, r2      r4 = 91
i4   STORE r4, [2]         spill r4 to memory
i5   LOAD  r5, [2]         reload it
i6   SUB   r6, r5, r1      r6 = 85    <- LOAD-USE on r5
i7   BEQ   r6, r4, i14     85 == 91? no -- falls through
i8   ADD   r7, r6, r1      r7 = 91
i9   MUL   r3, r7, r2      r3 = 637
i10  BEQ   r7, r4, i13     91 == 91 -- TAKEN, squashes i11 and i12
i11  SUB   r6, r3, r1      wrong path
i12  STORE r6, [5]         wrong path
i13  NOP                   landing pad
i14  STORE r7, [4]         mem[4] = 91
i15  HALT
i16  LOADI r7, -1          must never be fetched
```

What each part plants:

- **Both bypass paths in one line.** `i2` needs `r2` from `i1` (distance 1,
  EX/MEM) and `r1` from `i0` (distance 2, MEM/WB).
- **The load-use hazard.** `i4` spills `r4` and `i5` reloads it, so `i6` asks for
  a value that is still inside the memory port. This is the one stall forwarding
  cannot remove, and it costs exactly one bubble.
- **A branch that falls through** (`i7`) and **a branch that is taken** (`i10`),
  the second squashing the two instructions behind it.
- **A squash that outranks a stall.** `i11` depends on `i9`, which is still in
  MEM when `i10` resolves. With forwarding off, `i11` is stalling in ID on the
  very cycle the branch kills it — so the flush has to win over the stall.
- **Traps for a mis-timed pipeline.** The wrong-path pair `i11`/`i12` would write
  `r6` and `mem[5]` if they escaped the squash, and `i16` would clobber `r7` if
  the `HALT` failed to seal the front end. A timing bug therefore shows up in the
  architectural dump, not only in the cycle count.

## Reading the pipeline diagram

One row per cycle, one column per stage:

```text
cyc |  IF  ID  EX MEM  WB | event
  7 |  i6  i5  i4  i3  i2 |
  8 |  i7  i6  i5  i4  i3 | stall: load-use on r5
  9 |  i7  i6  --  i5  i4 |
 10 |  i8  i7  i6  --  i5 |
```

A cell names the instruction in that stage during that cycle; `--` means the
stage is empty — the pipeline filling or draining, a bubble a stall pushed in, or
a slot a taken branch squashed.

The event column names the decision made **during** that cycle, so its effect
appears in the row *below* it, the way a timing diagram drawn by hand reads.
Above: at cycle 8 the load `i5` is in EX and its consumer `i6` is in ID, so the
hazard unit holds `i6`; at cycle 9 `i6` has not moved and EX has a bubble; at
cycle 10 `i6` is in EX and the load is in WB, where the MEM/WB path can reach it.
One bubble, exactly as promised.

## The counters

`cycles`, `instructions retired`, `stall cycles`, `bubbles`, `squashed
instructions`, and `forwards` — split by which path served them, EX/MEM or
MEM/WB. IPC is printed as a truncated **permille** integer: there are no floats
in this host environment, and `636` says everything `0.64` would.

The counters check each other. Every cycle the EX stage either holds an
instruction that will go on to retire, or holds nothing, so
`cycles - retired == bubbles` must hold on both runs, and the program prints the
identity rather than asking to be believed.

By hand, forwarding on: `i2` takes one bypass on each path, `i3` the same, `i4`
one from EX/MEM, `i6` one from MEM/WB (the reloaded `r5`), `i7` one from EX/MEM,
`i8` one from MEM/WB, `i9` one from EX/MEM, `i10` one from MEM/WB — five and
five, which is what `forwards: 10 (EX/MEM 5, MEM/WB 5)` reports.

## Output shape

The program listing, then the two runs — each a full cycle-by-cycle diagram
followed by its counters — then the architectural dumps of both runs side by
side, the three identity verdicts, and the headline. Everything on stdout is the
program's own; there is no input.

What this adds to the corpus: the corpus's first microarchitecture simulation —
a five-stage in-order pipeline with RAW hazard detection against the instructions
in flight, EX/MEM and MEM/WB forwarding, an unavoidable load-use stall that no
bypass can cover, branch squashes on a taken branch, and an A/B comparison whose
architectural results must match while the cycle counts differ. It is the first
castle whose subject is *timing* rather than a computed answer. The two
configurations must have different cycle counts while producing identical
register and memory states; the simulator checks and prints both facts. It leans
on the `testapi-array` tier
for a mutable code array, register file and memory that every stage writes
through in place, and on `elab`'s `match!` and `widen_sum!` to dispatch a
label-generated opcode sum, an operand's three possible provenances (register
file, EX/MEM latch, MEM/WB latch), the hazard unit's verdict, and a branch's
redirect — with equality, strict order and a binary long division for the
permille IPC all derived in Kio from the single comparison `leq_i32` the tier
supplies.

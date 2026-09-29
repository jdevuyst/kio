# os_scheduler_sim

A small operating-system kernel: a preemptive round-robin scheduler running on
top of demand-paged memory. Four processes arrive on a staggered schedule and
run burst programs of CPU work and page references. A reference to a page that
is not resident traps into the pager, which takes a frame by the CLOCK
(second-chance) replacement policy and blocks the process while the page is
fetched.

The two subsystems are coupled, and that coupling is the point: paging decides
which processes the scheduler is *allowed* to run, and the schedule decides
which pages get their second chance before the hand reaches them. Neither can
be understood alone.

There is no stdin fixture. The process table, the programs, and the machine
constants are compiled into the program.

## The machine

| | |
| --- | --- |
| processes | 4 |
| virtual pages per process | 4 (`vp0` … `vp3`) |
| physical frames | 4 |
| quantum | 3 ticks |
| fault service latency | 3 ticks |
| replacement policy | CLOCK (second chance), with a rotating hand and a per-frame reference bit |

Four frames for nine distinct pages, so eviction is forced rather than
incidental. A page is named by the pair `(pid, vp)`; pages of different
processes never alias.

## The process fixture

Each process runs a **burst program** — a list of phases. A phase is either
`cpuN` (occupy the CPU for `N` ticks) or `readV` (reference virtual page `V`
exactly once).

| pid | arrives | program |
| --- | --- | --- |
| p0 | tick 0 | `read1 cpu2 read0 cpu1 read1 read2 cpu2` |
| p1 | tick 1 | `read1 cpu3 read2 read1 cpu1` |
| p2 | tick 2 | `cpu2 read0 read3 cpu1 read0 cpu2 read3` |
| p3 | tick 6 | `read2 cpu4 read2 read0` |

Every process revisits a page it touched earlier — the re-reference the CLOCK
reference bit exists to protect — and the arrivals are staggered so the ready
queue fills up while the first arrivals are away servicing their faults.

## What a tick does

1. **Wake** every process whose page has arrived, onto the tail of the ready
   queue, in the order the faults were taken.
2. **Admit** every process arriving on this tick.
3. **Dispatch**: if the CPU is free, take the head of the ready queue and give
   it a fresh quantum. The incoming process is charged one context switch — so
   a process's switch count is the number of times it was put on the CPU.
4. **Charge** the tick to everyone else: queued processes wait, blocked
   processes block.
5. **Run** the process on the CPU for one tick:
   - a `cpu` phase pays off one of the ticks it owes;
   - a `read` of a resident page costs the tick and sets the page's reference
     bit;
   - a `read` of a page that is *not* resident **traps**. The trap costs the
     tick, the pager evicts a frame and loads the page, and the process blocks
     for the service latency. Its program counter does not move, so the same
     reference is retried when it comes back — by which time the page is
     usually, but not always, still there.
6. **Settle** the CPU: a process out of program retires, one out of quantum
   goes back on the ready queue, anything else keeps the CPU.

A fault therefore costs the faulting process `1 + 3` ticks: one trap tick, then
three blocked. Frames are not reclaimed when a process retires; its pages stay
resident until the hand reaches them like any other page.

## Reading the output

- **`config`** — the machine above.
- **`-- process fixture --`** — the table above, printed by the program so the
  trace can be read without this file.
- **`-- trace (first 26 ticks) --`** — one line per tick: the tick, the process
  on the CPU (`--` when none), the scheduler's decision (`running` / `preempt`
  / `blocked` / `retired` / `idle`), and what the CPU actually did. A fault line
  names the frame the page landed in and the page that frame gave up, so the
  eviction order is readable straight off the trace. The trace is truncated at
  26 ticks; the run continues past it.
- **`-- processes --`** — per process: arrival and completion ticks;
  `turn`around (ticks in the system, arrival through completion inclusive);
  `wait` (ticks on the ready queue); `cpu` (productive ticks); `block` (ticks
  waiting for the pager); `switch` (times put on the CPU); `fault` and `hit`
  (page references that missed and that found their page resident).
- **`-- totals --`** — system-wide counters, and the hit ratio as a **permille**
  integer: hits per thousand page references, truncated. This host tier has no
  floats, and no division either — the ratio is computed by a long division
  written in Kio (`os/num.div_mod`), which doubles the divisor until it passes
  the dividend and rebuilds the quotient bit by bit on the way back out.
- **`accounting`** — a check, not decoration. It holds only if two independently
  accumulated identities agree: per process, `turn == cpu + fault + block +
  wait` (every tick between arrival and completion was spent in exactly one of
  those four ways, a fault costing exactly one trap tick); and system-wide,
  `sum(cpu) + sum(fault) + idle == ticks` (every elapsed tick was spent running
  some process, trapping for one, or idle). A model that dropped a tick anywhere
  would print `MISMATCH`.
- **`-- frame table (final) --`** — the raw state the replacement policy left
  behind: each frame's owning process, the virtual page it holds, and its
  reference bit.

## Structure

| module | |
| --- | --- |
| `os/config` | the machine constants |
| `os/num` | integer vocabulary the tier omits: equality and strict order from `leq_i32`, and long division |
| `os/text` | column formatting — the tier cannot measure a string, so numbers are padded from their digit count |
| `os/proc` | the `Phase` sum and the process control block |
| `os/fixture` | the process table |
| `os/memory` | frame table, page table, and the CLOCK hand |
| `os/queue` | the ready queue, the blocked list, and the completion order |
| `os/trace` | the `Event` and `Outcome` sums, and the per-tick trace |
| `os/sched` | the tick loop |
| `os/report` | the transcript |

What this adds to the corpus: the corpus's first OS kernel simulation —
preemptive round-robin scheduling coupled to demand paging with CLOCK
second-chance eviction, where the two subsystems interact through faults and
blocking. It is the first castle to run three label-generated sums as a working
vocabulary rather than a demonstration (`Phase` drives execution, `Event` and
`Outcome` split "what the CPU did" from "what the scheduler decided", all three
dispatched with `match!`), the first to hold host arrays of a sum type and to
nest them (`Array(Array(Phase))` is the program table), and the first to derive
a full integer vocabulary — equality, strict order, and division — from a tier
whose only comparison is `leq_i32` and whose arithmetic stops at multiplication.
The program checks itself: two independently accumulated tick identities must
agree, so a lost tick anywhere in the model surfaces as `MISMATCH` rather than
as a plausible-looking number.

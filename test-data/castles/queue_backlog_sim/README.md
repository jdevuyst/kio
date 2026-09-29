# queue_backlog_sim

`queue_backlog_sim` models a fixed seven-tick service window. Each tick adds a
known number of jobs, appends those jobs to the queue POC's FIFO structure, then
dequeues as many jobs as that tick's service capacity allows. Jobs carry a
ticket number, arrival tick, and effort score; the simulation totals served
jobs, final backlog, peak backlog, wait units, and completed effort.

The case has no `input.stdin`; the schedule is compiled into the Kio modules:

- arrivals by tick: `3, 0, 4, 1, 0, 2, 0`
- service capacity by tick: `1, 2, 1, 3, 2, 1, 4`

Stdout is a compact report. `ticks` is the number of simulated ticks,
`arrivals` is the number of jobs enqueued, `served` is the number dequeued,
`final_backlog` is the queue depth after the last tick, `max_backlog` is the
largest depth after arrivals were applied, `wait_units` is the sum of
`service_tick - arrival_tick` for served jobs, and `effort_units` is the sum of
the served jobs' effort scores.

What this adds to the corpus: a dependency-integrated queue simulation that
uses the queue POC as a real FIFO data structure, performs recursive
enqueue/dequeue service loops with `rec(loop)`, carries labeled job and state
records, and runs through the namespaced `testapi-arith-collection` host
surface without stdin.

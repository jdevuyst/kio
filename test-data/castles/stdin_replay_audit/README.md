# stdin_replay_audit

`stdin_replay_audit` replays a small line-oriented audit transcript from
`input.stdin`. The fixture uses one command per line:

- `BEGIN` opens the audit window.
- `CREDIT 12`, `CREDIT 7`, and `DEBIT 5` adjust the balance while the window is
  open.
- `CHECK` records a checkpoint while the window is open.
- `END` closes the audit window.

The program validates command ordering while it accumulates state. A
transaction before `BEGIN` or after `END`, a duplicate marker, an unknown
line, or a missing final `END` increments the error count. Stdout is the
deterministic audit report: lines read, transaction totals, checkpoints,
final balance, error count, and the final status.

What this adds to the corpus: this is a compact-to-medium stdin replay workflow
with fixture-driven control flow, sum dispatch over `read_ascii_line()`, labeled
state records, recursive `rec(loop)` input processing, and the `compute-main`
environment through the `testapi-compute` runner protocol.

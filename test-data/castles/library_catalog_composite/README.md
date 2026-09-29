# library_catalog_composite

A branch library's circulation desk, run for one day. The program composes four
repository libraries — `dict`, `list`, `result`, and `elab` — into a single
application workflow: `dict` holds the two indexes, `list` holds every working
collection, `result` carries every fallible operation, and `elab` supplies the
`match!` / `widen_sum!` vocabulary the domain sums are built and taken apart
with.

Everything is keyed by integers. The host tier this castle runs on
(`testapi-arith-collection`) has no string comparison and no division, so
call-numbers and member ids are `I32` compared with `eq_i32` / `lt_i32`, and
strings only ever get built (`string_concat`) for display.

## The domain

- **The catalog** is a `dict` keyed by call-number. Each entry is a labeled
  product `{ title, copies_total, copies_out }`.
- **The roster** is a second `dict` keyed by member id. Each member is
  `{ member_id, fines, held }`, where `held` is a `list` of the call-numbers
  that member currently has on loan.
- **The hold log** is an append-only `list` of `{ holder, held_call }`
  reservations.
- **Fines** accrue at 25 per day late, charged on return and accumulated per
  member.

Every operation that can fail returns a `result` value. The five failures are
`unknown_call`, `unknown_member`, `no_copies`, `not_held`, and
`copies_available`. None of them is fatal: the driver folds a failure back into
the unchanged state, appends it to an audit list, and carries on to the next
operation.

The rules:

- **CHECKOUT** fails when the call-number or the member is unknown, or when
  every copy is already out.
- **RETURN** fails when the member does not hold that call-number. A successful
  return shelves the copy and charges `days_late * 25`.
- **HOLD** is a reservation, so it only makes sense once every copy is lent out;
  placing one while a copy is still on the shelf fails with
  `copies_available`.
- **REPORT** never fails. It emits a one-line snapshot of the current state.

Both indexes are consulted before either is written, so an operation naming an
unknown member never touches the catalog.

## The fixture

Six titles (call-numbers 201-206, between one and three copies each), four
members (101-104), and 24 operations, all written in source in
`workdir/catalog/fixtures.kio`. There is no `input.stdin`: the program reads no
input.

Six of the 24 operations are expected to fail, one of each failure kind except
`copies_available`, which fires twice:

| op | why it fails |
| --- | --- |
| 04 `CHECKOUT 104 202` | Neuromancer's single copy is already out |
| 09 `RETURN 102 203` | member 102 never borrowed Solaris |
| 12 `HOLD 101 204` | Ubik is still on the shelf |
| 13 `CHECKOUT 199 201` | member 199 is not enrolled |
| 14 `CHECKOUT 101 299` | call-number 299 is not in the catalog |
| 22 `HOLD 102 206` | Embassytown is still on the shelf |

## Reading the output

- **`== transcript ==`** — one line per operation in order:
  `<n> <OP> m=<member> call=<call> [late=<days>] -> ok: <what changed>` or
  `-> err: <why>`. A CHECKOUT or RETURN reports the book's copy count after the
  operation (`203 out 1/2`); a RETURN also reports the fine charged; a HOLD
  reports how many reservations that call-number now carries; a REPORT prints
  the running totals.
- **`== shelf ==`** — the final catalog in ascending call-number order:
  copies out over copies total, the number of reservations recorded against the
  call-number, whether a copy is still on the shelf, and the title.
- **`== fines ==`** — the final roster in ascending member id: fines owed and
  how many books that member still has on loan, then the total across all
  members. Four copies are out at the end and each member is holding exactly
  one, so the two tables cross-check.
- **`== audit ==`** — how many times each failure kind fired, and the failed
  operation count out of the total.

## Running

```sh
kio fmt
kio check
kio build js
```

The case runs under `--protocol testapi-arith-collection` (the only line in
`run.args`) and exits `0`.

## Dependency wiring

The four dependencies are declared in `workdir/*.dep.kio` and materialized with
`kio dep fetch`; the materialized trees are committed alongside them. The
libraries reach the host in two different ways, and the castle uses both:

- `dict/dict` and `list/list` declare host **functions** (`loop`, `add`,
  `eq_i32`, `string_concat`, ...). Those are rebound onto this package's own
  host surface with a `rehost` clause in the dependency file, pointing at the
  small `dict_host` / `list_host` modules that forward each name to
  `testapi/arith`, `testapi/iter`, and `testapi/text`. Without the rehost, the
  library's `Bool` would be a *different* host type from `testapi.Bool` and no
  value could cross between the library and this package.
- `elab/testapi` (and the three copies of it that arrive nested under `dict`,
  `list`, and `result`) declares host **types** only. Those are role types that
  never cross into this package's code, so they are simply named in the
  `bridge` block and satisfied as ordinary atomic bindings.

What this adds to the corpus: the first composition with four direct
dependencies, and the first to combine `dict`, `list`, `result`, and `elab` in
one application workflow. It makes each of them load-bearing —
`dict` for both indexes, `list` for the operation queue, the per-member loans,
the hold log, the transcript, and the audit list, `result` for every fallible
step and the `bind` / `map` / `fold` plumbing between them, and `elab` for the
`match!` and `widen_sum!` the two domain sums are built and dispatched with.
It is also the first castle to need *both* dependency-wiring styles at once: a
`rehost` clause for the libraries whose modules declare host functions, and a
plain `bridge` entry for the ones whose modules declare only role types. That
makes it the corpus's test of whether four separately-authored packages, each
vendoring its own copy of `elab`, can be threaded onto one host surface and do
real work together under a single application workflow.

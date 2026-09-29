# query_plan_engine

A relational query engine over a small harbor dataset. Query plans are
built as relational-algebra trees, printed as indented plan trees, and
then interpreted bottom-up over three in-source tables: scans, nested-loop
joins, predicate filters, projections, grouped sums and counts, an
insertion sort, and a limit. Every query prints its plan, the relation it
produced as a column table, the row count, and what the run cost.

The program reads no input; the dataset is a fixture in the source.

## The data

Three base tables, twenty rows, in `query/fixtures`. Every key the engine
joins, groups, or orders on is an integer; every name is display text
carried beside its key.

**ships** - six ships, each moored at one berth. Draft is in decimetres.

| s.id | ship | flag | draft | s.berth |
| --- | --- | --- | --- | --- |
| 1 | Aurora | PA | 92 | 1 |
| 2 | Borealis | LR | 68 | 2 |
| 3 | Cygnus | NO | 115 | 3 |
| 4 | Dorado | PA | 74 | 2 |
| 5 | Eos | GR | 51 | 4 |
| 6 | Fulmar | LR | 103 | 1 |

**berths** - four berths. Depth is in decimetres, so it compares directly
with a ship's draft. A berth carries its zone twice: `b.zone` is the zone's
id, which the engine groups on, and `zone` is the label, which the report
prints.

| b.id | depth | zone | b.zone |
| --- | --- | --- | --- |
| 1 | 120 | North | 10 |
| 2 | 70 | North | 10 |
| 3 | 130 | East | 20 |
| 4 | 45 | South | 30 |

**manifests** - ten cargo lots, each naming its ship by id.

| m.ship | cargo | tons |
| --- | --- | --- |
| 1 | grain | 320 |
| 1 | ore | 140 |
| 3 | ore | 480 |
| 3 | grain | 260 |
| 3 | fuel | 60 |
| 2 | timber | 210 |
| 4 | fuel | 190 |
| 6 | ore | 350 |
| 6 | fuel | 120 |
| 5 | timber | 90 |

## The engine

A **relation** is a list of rows plus the column list that names its
shape. A **row** is an association list of cells, and a cell binds a
catalog column to a datum - an integer, or text carrying its own display
width. A **plan** is a tree of operators over relations: `Scan`, `Filter`,
`Join`, `Project`, `GroupSum`, `GroupCount`, `OrderBy`, and `Limit`.
Predicates are their own tree - a column against a literal, a column
against another column, `and`, `or`, `not` - so a filter is data the
engine walks rather than a function it calls.

Three consequences of the host surface this package declares are worth
knowing when reading the output, because they are what shape the design:

- The host offers no string comparison, so **every comparison is on
  integers**. A ship names its berth by id and a berth names its zone by
  id; the labels ride along for the report only. A predicate, join key,
  or sort key aimed at a text column is well-defined and simply never
  matches, rather than inventing an order for data the host cannot
  compare.
- The host offers no way to measure a string, so **a text datum carries
  its own width**, the way a column store carries the length beside the
  characters. That width is what the report writer pads columns from.
- The host offers no division, so the printed width of an integer is
  found by climbing decades rather than by dividing it down.

## The queries

1. **Q1 - deep-draft ships.** The ships drawing 90 decimetres or more,
   deepest first, showing name, flag, and draft. Filter, sort, project.
2. **Q2 - berth assignments.** Each ship joined to the berth it is moored
   at, shallowest berth first. Two tables, joined on the ship's berth id.
   The two pairs of berths that tie on depth come out in ship order -
   the sort is stable.
3. **Q3 - over-draft alerts.** The same join, filtered by a predicate that
   compares *two columns of the joined row* - the berth is shallower than
   the ship draws - so the alert falls out of the join itself. Two ships
   draw more water than their berth has.
4. **Q4 - cargo tonnage by zone.** Three tables: each manifest reaches its
   ship, the ship reaches its berth, and the berth names the zone the
   tonnage is credited to. The tonnage is summed per zone and the zones
   come out heaviest first. Zone 10 is North, 20 is East, 30 is South.
5. **Q5 - busiest ships.** The manifests counted per ship, most first, cut
   to the top three. Ships 1 and 6 tie on two manifests each and keep
   their first-seen order.
6. **Q6 - priority cargo.** Manifests joined to their ship, then filtered
   by a compound predicate - over 300 tons, or 90 tons and under, the two
   ends of the scale that get mishandled, and nothing at all from ship 6 -
   and ordered heaviest first. Ship 6's 350-ton lot is heavy enough to
   qualify and is dropped anyway, by the `not`.

## The output

Each query prints four sections.

- **plan** - the plan tree, one operator per line, children indented under
  their parent. This is the tree the evaluator is about to walk.
  `ordered:` says whether the rows will come out in a known order: only
  `OrderBy` establishes one, and `Filter`, `Project`, and `Limit` pass
  their child's through. Q3 has no `OrderBy` anywhere, so it reports
  `false` and its rows arrive in join order.
- **result** - the relation, as a column table. The header and the widths
  come from the catalog; numbers are set flush right under their heading,
  text flush left.
- **rows** - how many rows the query returned.
- **stats** - what the run cost. `scanned` counts the base-table rows the
  `Scan` operators read: Q4 reads all twenty. `compares` counts every data
  comparison the engine attempted - one per predicate leaf per row, one
  per pair for a nested-loop join (Q2's join of six ships against four
  berths is 24 of them), one per step of the insertion sort, and one per
  bucket probed while grouping. The predicate evaluator does not
  short-circuit, so a row costs its predicate's full leaf count either
  way: Q6's three-leaf predicate over ten joined rows is exactly 30
  comparisons.

The numbers are the point: they show the engine really walked the data it
claims to have walked, and they change when the plan does.

What this adds to the corpus: the first relational-algebra evaluator -
plan ASTs interpreted over record tables, with joins, aggregation and
sorting expressed over lists without host division or string equality.
The corpus already interprets expression ASTs and walks record fixtures,
but not with a *relation* as the value that flows between operators: a
schema travelling with its rows, one generic `List(A)` reused as row,
table, column list, and group accumulator, an evaluator that threads a
cost budget through a nested-loop join, a stable insertion sort, and
association-list aggregation, and a report writer that lays out padded
columns from a catalog on a host that cannot measure a string.

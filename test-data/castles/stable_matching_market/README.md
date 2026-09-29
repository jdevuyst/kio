# Stable matching market

Four participants on each side have strict, complete preference lists. A
deferred-acceptance engine processes a FIFO queue of free proposers. A
recipient holds the best proposal received so far, returning displaced
partners to the queue; rejected proposers also return to try their next
choice. The engine indexes an inverse rank table for those decisions.

The fixture is fixed integer arrays in `workdir/market.kio`; there is no stdin
or random seed. Participants are numbered 0 through 3 on each side. Each row
below lists partners from most to least preferred:

| Participant | Left preferences | Right preferences |
| --- | --- | --- |
| 0 | 3, 1, 0, 2 | 2, 3, 1, 0 |
| 1 | 1, 3, 0, 2 | 2, 0, 3, 1 |
| 2 | 3, 0, 1, 2 | 1, 0, 3, 2 |
| 3 | 2, 1, 0, 3 | 3, 2, 1, 0 |

The program runs left-side proposals with ascending and descending initial
queue orders, then runs right-side proposals. Stdout reports proposal,
switch, and rejection counts and always renders the resulting pairs as
`L<number>-R<number>`. Rank totals use zero for each participant's first
choice. Changing the left queue order changes the event counts but preserves
its matching; changing the proposing side produces another stable matching.
The right-proposing partner map `(2,0,1,3)` is converted to its distinct inverse
`(1,2,0,3)` for the left-to-right report and certificates.

The certificate module independently checks the partner maps for a bijection
and scans the original preference rows for blocking pairs: unmatched pairs
whose members both prefer each other to their assigned partners. It does not
consume the engine's inverse rank table or proposal history. The identity
matching supplies a negative control with six blocking pairs, and a map
containing a duplicate recipient supplies an invalid-matching control.

Exhaustively checking all 24 possible bijections yields exactly three stable
matchings, written as right partners for L0, L1, L2, and L3:
`(1,0,3,2)`, `(1,2,0,3)`, and `(1,2,3,0)`. The two proposing-side outcomes are
the corresponding side's best choices across those three matchings. This
bounded enumeration independently establishes the expected fixture results;
the Kio runtime certificates verify validity and stability of each run.

The package uses the `testapi-array` protocol and a materialized dependency on
the shared elaborator library for lazy conditionals. Modules separate the
market fixture, proposal engine, certificates, and presentation.

What this adds to the corpus: a preference-stability algorithm whose workqueue
grows through rejection and displacement, with independent blocking-pair
certificates, proposal-order variation, and two-sided preference tradeoffs.
Its matching criterion depends on individual ordered preferences rather than
aggregate scores or price priority.

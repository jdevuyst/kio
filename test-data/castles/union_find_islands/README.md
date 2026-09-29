# union_find_islands

`union_find_islands` models a fixed 3 by 3 map as a small graph and runs a
union-find pass over the land cells. The program builds mutable host arrays for
the land mask, edge endpoints, parent links, and component sizes, then prints a
compact trace of accepted and skipped edges plus the final island roots.

This castle does not read stdin. Its fixture is embedded in the source: land
cells are indexed row-major from `0` to `8`, with land at `0`, `1`, `3`, `5`,
`7`, and `8`. The edge list includes both land-land and water-touching
adjacencies so the diagnostic counters exercise both union and skip paths.

Stdout first prints the map, then the edge-processing summary, then one line
per land cell showing its representative root and one line per root showing
the component size. The final checksum combines the number of union operations,
skipped edges, and discovered island roots.

What this adds to the corpus: this is a graph/data-structure workflow centered
on mutable arrays and union-find state. It is not a route search or dynamic
programming table: the important behavior is in alias-preserving mutation of
parent and size arrays, repeated root walks through `rec(loop)`, and component
diagnostics emitted after graph contraction.

# Polygon triangulation

This program triangulates small integer-coordinate polygons by ear clipping.
It keeps predecessor and successor arrays for the live boundary, tests convexity
with cross products, and rejects ears containing another active vertex,
including points on the candidate triangle's boundary. Removing an ear updates
both links and its active flag; the remaining three vertices form the last
triangle. Clockwise inputs are reversed before clipping.

The supported domain is a single simple polygon with 3–12 distinct vertices,
coordinates from -20 through 20, and no collinear consecutive triple. The first
point is not repeated to close the boundary. Validation checks these bounds,
duplicates, collinear corners, nonadjacent edge intersections, and zero area.
The fixed arrays live in `workdir/polygon/fixtures.kio`; there is no stdin.
The rectangle, concave arrow, deeply notched polygon, and clockwise arrow cover
different clipping decisions. Five invalid arrays exercise domain rejection as
ordinary successful reports.

Each triangle is printed as three zero-based vertex indices in the normalized
point array. `input-area2` is the original signed doubled area; `area2` is the
sum of the output triangles' signed doubled areas. The separate certificate
module checks `n - 2` triangles, valid vertex indices, positive triangle areas,
equal total area, and directed edge counts. Every polygon boundary edge occurs
once in its direction; each internal edge occurs once in each direction.
`boundary` and `internal` report the edge counts. A damaged rectangle result
repeats one triangle: its count and area still agree, but its edge certificate
fails. This demonstrates a check that the aggregate area alone cannot supply.

The package uses the `testapi-array` runner protocol and the direct `elab`
dependency for ordinary conditionals and sum dispatch. All eight host targets
are configured; the expected output is deterministic.

What this adds to the corpus: geometric topology changes during a mutable
ring walk, exact integer containment decisions, orientation normalization,
and a separate combinatorial certificate over the generated triangulation.
The certificate examines output edges independently of the clipping state.

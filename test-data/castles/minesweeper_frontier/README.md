# minesweeper_frontier

`minesweeper_frontier` models a fixed Minesweeper-like board and analyzes the
frontier around the currently revealed cells. It builds the mine mask and reveal
mask in Kio, computes every cell's neighboring mine count, classifies hidden
cells by whether they border a revealed cell, and prints a compact
recommendation report.

The program reads no stdin. The board fixture is embedded in the source as a
5-by-6 grid with six mines and eight revealed cells. Stdout reports the board
summary, frontier class counts, the selected safe frontier recommendation, and a
clone check showing that mutating a copied reveal plan leaves the original array
unchanged.

What this adds to the corpus: this is a medium mutable-array grid analyzer. It
uses `testapi-array` with `Array[T]` for boolean masks, integer count grids, and
frontier lists; exercises 2D neighbor scans, array mutation, clone behavior, and
array push/swap list handling; and is distinct from union-find or dynamic
programming array cases because the work is local grid classification and
recommendation over a Minesweeper frontier.

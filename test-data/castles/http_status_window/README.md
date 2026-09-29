# http_status_window

`http_status_window` is a medium structured-text castle that owns one fixed
ASCII access-log-like window. Each row uses fixed columns:

```text
NN|MMMM|/path |SSS|BBBB
```

The program slices the known fields from four checked-in rows, classifies each
HTTP status by family, and prints a compact report. It does not read stdin, so
there is no `input.stdin` fixture. The run uses `--protocol testapi-text`,
which supplies the namespaced `testapi` string, formatting, and printing
helpers.

Stdout contains a readable summary: the raw window length, row count, first-row
integrity check, the status and family chains, and one rendered line per
record. The checksum-like summary is intentionally built from fixed slices and
host text helpers rather than a recursive parser or arithmetic loop.

What this adds to the corpus: this castle covers a hand-structured text scanner
over a fixed-width log window using only `testapi-text`. It stresses namespaced
host boundaries, string slicing/equality, label-shaped records, cross-module
composition, and multi-backend emission without relying on stdin, loops, arrays,
or arithmetic helpers.

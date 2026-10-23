#!/bin/sh
# Long decimal coefficients can cancel a large exponent before IEEE rounding.
# Generate the ordinary source to keep this exact regression compact on disk.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/float_literal_scale_cancellation.pkg.kio "$work/"
cd "$work"
awk 'BEGIN {
  print "module main; host type Float role(f32); host type Double role(f64);"
  printf "equiv whole_scale() { 1"
  for (i = 0; i < 660000; i++) printf "1"
  print ".0e-660000(Float); 1.1111111640930176(Float) }"
  printf "equiv fraction_scale() { -0."
  for (i = 0; i < 660000; i++) printf "0"
  print "1e660001(Double); -1.0(Double) }"
}' > main.kio
"$KIO_BIN" test

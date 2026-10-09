#!/bin/sh
set -eu
cd workdir

"$KIO_BIN" build "$KIO_TARGET"
"$KIO_RUNNER" --protocol testapi-float-f32-f64 --package-name float_string_range \
  "out/$KIO_TARGET" > out/numeric.stdout

# Both float widths represent +/-2^64 exactly. Compare numeric values, not
# the host's choice of decimal versus scientific notation.
LC_ALL=C awk '
  {
    expected = NR % 2 ? 18446744073709551616 : -18446744073709551616
    if (NR > 4 || $0 !~ /^[+-]?[0-9]+([.][0-9]+)?([eE][+-]?[0-9]+)?$/ || $0 + 0 != expected) {
      print "unexpected float string: " $0
      failed = 1
    }
  }
  END {
    if (NR != 4 || failed) exit 1
    print "float strings preserve values"
  }
' out/numeric.stdout

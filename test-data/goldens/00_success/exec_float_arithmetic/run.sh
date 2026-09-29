#!/bin/sh
# Typechecks host-declared floating-point arithmetic without relying
# on runner-supplied bodies for those host functions. Three cases:
#
#   (a)  1.5 + 2.5       = 4               exact double add.
#   (b)  0.1 * 3.0       = 0.30000000000000004
#                                                classic IEEE 754 double
#                                                imprecision; the
#                                                canonical decimal form
#                                                of the closest double.
#   (c)  0.1 + 0.2       = 0.30000001192092896
#                                                f32 precision rounded
#                                                to f32 width on each
#                                                op and on the sum.
#
# Each numeric literal is bare — its type is pinned by the
# `add_f32` / `add_f64` / `mul_f64` parameter it flows into. The
# host functions are declared package requirements, not canonical
# runner helpers.
set -u
cd workdir || exit
"$KIO_BIN" check

#!/bin/sh
# Typechecks host-declared arithmetic over small-integer roles without
# relying on runner-supplied bodies for those host functions. Four wrap
# cases:
#
#   (a)  i8::MAX + 1   = -128       add overflow → wraps to MIN
#   (b)  50 * 50       =  -60       2500 mod 256 = 196 → signed = -60
#   (c)  u16::MAX + 1  =  0         unsigned add overflow → wraps to 0
#   (d)  0 - 1         =  65535     unsigned sub underflow → wraps to MAX
#
# Each numeric literal is bare — its type is pinned by the
# arithmetic fn it flows into. The host functions are declared
# package requirements, not canonical runner helpers.
set -u
cd workdir || exit
"$KIO_BIN" check

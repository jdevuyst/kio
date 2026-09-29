#!/bin/sh
# Dot-led one-dot op tokens are reserved as a family, not just the
# structural spellings `.` and `.>`. `.###$` starts with `.` but
# contains only one dot, so it is held back for future dot-led syntax.
set -u
cd workdir || exit
"$KIO_BIN" check

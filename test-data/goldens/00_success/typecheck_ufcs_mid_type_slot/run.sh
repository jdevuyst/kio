#!/bin/sh
# The receiver fills a value slot before an explicit type slot in
# the same flat call.
set -u
cd workdir || exit
"$KIO_BIN" check

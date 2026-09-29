#!/bin/sh
# A java `namespace` segment that is a Java reserved word is a build
# error (exit 40) naming the segment.
set -u
cd workdir || exit
"$KIO_BIN" build

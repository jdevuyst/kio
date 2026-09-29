#!/bin/sh
# A `bridge { … }` glob names module-path-shaped value entries
# (`<module>` or `<module>.<leaf>`). A bare uppercase type name like
# `Widget` is not a value-name-shaped segment, so it fails the
# value-name lexical rule during package-file parsing — exit 11, a
# parse error, before any bridge-contract (dead-glob, exit 20) check
# runs. Pins that tier.
set -u
cd workdir || exit
"$KIO_BIN" check

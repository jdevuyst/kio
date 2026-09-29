#!/bin/sh
# A variant=package snippet whose body doesn't parse as a
# package file (here, regular module syntax in a package-file
# slot) fails with the parser's exit code, which kio doc check
# surfaces as a DocError.
set -u
"$KIO_BIN" doc check

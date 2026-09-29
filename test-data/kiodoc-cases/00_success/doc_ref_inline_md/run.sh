#!/bin/sh
# [`name`] intra-doc references in .md prose resolve against the
# package's package file when one exists near the .md file.
# The package file is at pkg.pkg.kio (in the same directory as input.md).
set -u
"$KIO_BIN" doc check

#!/bin/sh
# `kio test <file>` runs only the named file's equivs. With two
# package-less modules each carrying one equiv, selecting `a.kio`
# discharges `ea` and leaves `eb` untouched (specs/cli.md § kio test:
# a selector is a module name or a filename path).
set -u
cd workdir || exit
"$KIO_BIN" test a.kio

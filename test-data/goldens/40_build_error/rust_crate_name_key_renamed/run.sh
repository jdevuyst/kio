#!/bin/sh
# The rust target key `crate_name` was renamed to `namespace`; the
# old spelling is a build error (exit 40) with a renamed-to hint.
set -u
cd workdir || exit
"$KIO_BIN" build

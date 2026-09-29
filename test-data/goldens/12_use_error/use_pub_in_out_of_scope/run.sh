#!/bin/sh
# `use` of a `pub(helper)` item from outside the `helper` subtree — exit 12.
set -u
cd workdir || exit
"$KIO_BIN" check

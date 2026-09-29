#!/bin/sh
# A {@} snippet inside a /// doc-comment that fails kio check
# is reported as a Kiodoc contract violation (exit 70).
set -u
"$KIO_BIN" doc check

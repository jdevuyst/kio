#!/bin/sh
# A broken [`name`] intra-doc reference in .md prose is reported
# as a Kiodoc contract error (exit 70). The package file declares
# `print` and `S` but not `nonexistent`.
set -u
"$KIO_BIN" doc check

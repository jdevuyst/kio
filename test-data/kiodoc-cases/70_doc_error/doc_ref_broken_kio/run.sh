#!/bin/sh
# A broken [`name`] intra-doc reference in a /// doc-comment is
# reported as a Kiodoc contract error (exit 70). The name `nonexistent`
# is not declared anywhere in the module.
set -u
"$KIO_BIN" doc check

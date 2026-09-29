#!/bin/sh
# [`name`] intra-doc references in /// doc-comment prose resolve
# against the surrounding module's scope (top-level items and imports).
set -u
"$KIO_BIN" doc check

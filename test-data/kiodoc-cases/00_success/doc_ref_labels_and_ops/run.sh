#!/bin/sh
# [`name`] intra-doc references resolve against label entry names and operator
# function names. Labels declared in `labels` blocks expose their entry names (e.g.
# [`some`], [`none`]) and operator declarations expose their underlying function
# name (e.g. [`add`] for `op _ + _ { impl add, };`).
set -u
"$KIO_BIN" doc check

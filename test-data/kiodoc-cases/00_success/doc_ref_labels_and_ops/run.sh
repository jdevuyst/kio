#!/bin/sh
# [`name`] intra-doc references resolve against nominal entry names and operator
# function names. Compact type groups expose their nominal entry names (e.g.
# [`Some`], [`None`]) and operator declarations expose their underlying function
# name (e.g. [`add`] for `op _ + _ { impl add, };`).
set -u
"$KIO_BIN" doc check

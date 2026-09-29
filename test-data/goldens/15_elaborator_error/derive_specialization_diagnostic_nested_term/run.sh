#!/bin/sh
# A fatal specialization diagnostic from a recursively required rule reaches
# the term-producing parent instead of becoming an ordinary failed subgoal.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check

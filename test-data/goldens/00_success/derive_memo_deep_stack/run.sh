#!/bin/sh
# Deep recursive `derive!` stack over the same candidate tuple. Normal
# CI treats this as correctness coverage; manual debug runs expose
# recursion depth, shared subgoal reuse, and candidate scans.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check

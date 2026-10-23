#!/bin/sh
# `atom!`'s target must DNF-normalize to a single arm; a multi-arm
# sum target is rejected. Sum-to-smaller-sum narrowing is `onto!` /
# `align!`'s job. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check

#!/bin/sh
# Import-style selectors keep operator and label identities distinct from values.
set -eu
cd workdir || exit
printf '%s\n' \
    ':sig op _ + _' \
    ':sig demo/ops.(op _ + _)' \
    ':sig arithmetic.(op _ .. _)' \
    ':sig arithmetic.(op _ (=) _)' \
    ':sig demo/ops.(varop [* *])' \
    ':src arithmetic.(op _ + _)' \
    ':doc arithmetic.(varop [* *])' \
    ':which op _ + _' \
    ':which arithmetic.(op _ + _)' \
    ':which choose' \
    ':refs arithmetic.(op _ + _)' \
    ':refs varop [* *]' \
    ':sig arithmetic.{field}' \
    ':sig arithmetic.field' \
    ':refs arithmetic.{field}' \
    ':refs demo/ops.fn' \
    ':refs demo/ops.pub' \
    ':sig demo/ops.op' \
    ':sig demo/ops.varop' \
    ':sig demo/ops.op _ + _' \
    ':sig demo/ops.(choose)' \
    ':sig demo/ops.(op _ + _, choose)' \
    ':sig demo/ops.(op _ - _)' \
    ':quit' | "$KIO_BIN" repl demo/main

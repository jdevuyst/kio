#!/bin/sh
# Import-style selectors keep operator and type alias identities distinct from values.
set -eu
cd workdir || exit
printf '%s\n' \
    ':sig op _ + _' \
    ':sig demo/ops.(op _ + _)' \
    ':sig arithmetic.(op _ (=) _)' \
    ':sig demo/ops.(op_var [* *])' \
    ':src arithmetic.(op _ + _)' \
    ':doc arithmetic.(op_var [* *])' \
    ':which op _ + _' \
    ':which arithmetic.(op _ + _)' \
    ':which choose' \
    ':refs arithmetic.(op _ + _)' \
    ':refs op_var [* *]' \
    ':sig arithmetic.Field' \
    ':sig arithmetic.field' \
    ':refs arithmetic.Field' \
    ':refs demo/ops.fn' \
    ':refs demo/ops.pub' \
    ':sig demo/ops.op' \
    ':sig demo/ops.varop' \
    ':sig demo/ops.op _ + _' \
    ':sig demo/ops.(choose)' \
    ':sig demo/ops.(op _ + _, choose)' \
    ':sig demo/ops.(op _ - _)' \
    ':quit' | "$KIO_BIN" repl demo/main

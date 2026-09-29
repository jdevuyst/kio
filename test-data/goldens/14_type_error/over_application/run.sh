#!/bin/sh
# Over-application: `f` has one value slot (`fn f(x: A)`); the call
# passes two value arguments. The remaining argument is rejected as a
# type error rather than reaching the internal-error tier. The dual
# under-application case is
# `arity_mismatch`; the too-few-product-components case is
# `within_tuple_partial_app`.
set -u
cd workdir || exit
"$KIO_BIN" check

#!/bin/sh
set -u

# Operator and left-splice callees need grouping before an outer call suffix.
"$KIO_BIN" fmt - <<'KIO'
module fmt_composite_callee; fn prefix_impl(value: Formtype) -> Formtype { value } op - __ { impl prefix_impl; }; fn operator_call() -> . { (- form_value)(context_arg) } fn splice_call() -> . { (context_callee.<form_receiver)(context_arg) }
KIO

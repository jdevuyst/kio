#!/bin/sh
set -u

# Operator receivers need grouping before a receiver-first UFCS splice.
"$KIO_BIN" fmt - <<'KIO'
module fmt_operator_splice_receiver; fn prefix_impl(value: Formtype) -> Formtype { value } op - __ { impl prefix_impl; }; varop [% %] { foldr form_push form_empty; }; fn normal_splice() -> . { (- form_value).>context_callee } fn variadic_splice() -> . { ([% form_left, form_right %]).>context_callee }
KIO

#!/bin/sh
set -u

# Unary operator receivers stay grouped; bracket atoms take UFCS calls directly.
"$KIO_BIN" fmt - <<'KIO'
mod fmt_operator_field_receiver; fn prefix_impl(value: Formtype) -> Formtype { value } op - __ { impl prefix_impl; }; op_var [% %] { foldr form_push form_empty; }; fn normal_access() -> . { (- form_value).>select } fn normal_update() -> . { (- form_value).>with(form_replacement) } fn variadic_access() -> . { ([% form_left, form_right %]).>select } fn variadic_update() -> . { ([% form_left, form_right %]).>with(form_replacement) }
KIO

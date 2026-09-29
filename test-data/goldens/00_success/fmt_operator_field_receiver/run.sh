#!/bin/sh
set -u

# Operator receivers need grouping before field-access and field-update suffixes.
"$KIO_BIN" fmt - <<'KIO'
module fmt_operator_field_receiver; fn prefix_impl(value: Formtype) -> Formtype { value } op - __ { impl prefix_impl; }; varop [% %] { foldr form_push form_empty; }; fn normal_access() -> . { (- form_value).?{form_field} } fn normal_update() -> . { (- form_value).!{form_field = form_replacement} } fn variadic_access() -> . { ([% form_left, form_right %]).?{form_field} } fn variadic_update() -> . { ([% form_left, form_right %]).!{form_field = form_replacement} }
KIO

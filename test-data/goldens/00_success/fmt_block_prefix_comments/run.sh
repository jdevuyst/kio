#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_block_prefix_comments;
fn call() { let result = choose! factory // call boundary
(()) { () }; result }
fn field() { choose! record // field boundary
.?{field} { () } }
fn update() { choose! record // update boundary
.!{field = value} { () } }
fn chained_call() { choose! factory // chained call boundary
(()).>select { () } }
fn leading_receiver() { choose! ((// leading receiver
receiver).>select.>>finish) { () } }
fn receiver_last() { choose! ((// receiver last
receiver).>>select) { () } }
fn trailing_receiver() { choose! receiver // trailing receiver
.>select.>finish { () } }
fn left_splice() { choose! select.<(receiver // left splice boundary
) { () } }
fn left_last_splice() { choose! select.<<(receiver // left last splice boundary
) { () } }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p'

#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_operator_operand_comments;
op _ + _ { impl add; }
op _ :: __ { impl cons; }
fn final_operand() { left + (right // final operand
) }
fn leading_operand() { left + (// leading operand
right) }
fn first_operand() { (// first operand
left) + right }
fn recursive_operand() { left :: (// recursive leading
middle :: right // recursive trailing
) }
fn grouped_operand() { left + (middle :: right // grouped trailing
) }
fn before_operator() { (left // before operator
) + right }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p')
test "$comments" = '// final operand
// leading operand
// first operand
// recursive leading
// recursive trailing
// grouped trailing
// before operator'
printf '%s\n' 'operator operand comments retained'

#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_label_header_comments;
fn construct(other: .) { {ready // construction equals
= other} }
fn update(row: Ready, other: .) { row.!{ready // update equals
= other} }
fn access(row: Ready) { row.?{m // access dot
. // access segment
ready} }
fn bind(row: Ready) { let .({ready // binding as
as // binding local
value}) = row; value }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
for comment in 'construction equals' 'update equals' 'access dot' 'access segment' 'binding as' 'binding local'; do
  test "$(printf '%s\n' "$formatted" | grep -F -c "// $comment")" -eq 1
done
printf '%s\n' 'label-header comments preserved'

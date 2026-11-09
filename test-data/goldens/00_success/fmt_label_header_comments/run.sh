#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_label_header_comments;
fn construct(other: .) { record { Ready.mk // construction head
(other) } }
fn update(row: Ready, other: .) { row.>with { Ready.mk // update head
(other) } }
fn access(row: Ready) { row.>select { m // access qualifier
. // access segment
Ready.get } }
fn bind(row: Ready) { let .(value // binding value
, other // binding local
) = row.>select { Ready.get; Other.get }; value }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
for comment in 'construction head' 'update head' 'access qualifier' 'access segment' 'binding value' 'binding local'; do
  test "$(printf '%s\n' "$formatted" | grep -F -c "// $comment")" -eq 1
done
printf '%s\n' 'row-head comments preserved'

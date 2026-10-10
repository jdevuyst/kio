#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_label_list_comments;
fn access(row: Ready) { row.?{ready // access closing
} }
fn bind(row: Ready) { let .({ready // binding closing
}) = row; ready }
fn empty_access(row: Ready) { (row.?{ // empty access inside
} // empty access outside
).?{ready} }
fn empty_update(row: Ready) { (row.!{ // empty update inside
} // empty update outside
).?{ready} }
fn access_boundary(row: Ready) { row // access receiver
.? // access opener
{ready} }
fn update_boundary(row: Ready) { row // update receiver
.! // update opener
{ready} }
fn bind_outer(row: Ready) { let .({ready} // binding outer close
) // binding equals
= row; ready }
fn neutral_bind(row: Ready) { flow! { let .({ready} // neutral binding outer close
) = row; ready } }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
test "$(printf '%s\n' "$formatted" | grep -F -c '// access closing')" -eq 1
test "$(printf '%s\n' "$formatted" | grep -F -c '// binding closing')" -eq 1
test "$(printf '%s\n' "$formatted" | grep -F -c '// empty access inside')" -eq 1
test "$(printf '%s\n' "$formatted" | grep -F -c '// empty access outside')" -eq 1
test "$(printf '%s\n' "$formatted" | grep -F -c '// empty update inside')" -eq 1
test "$(printf '%s\n' "$formatted" | grep -F -c '// empty update outside')" -eq 1
for comment in 'access receiver' 'access opener' 'update receiver' 'update opener' 'binding outer close' 'binding equals' 'neutral binding outer close'; do
  test "$(printf '%s\n' "$formatted" | grep -F -c "// $comment")" -eq 1
done
printf '%s\n' 'label-list closing comments preserved'

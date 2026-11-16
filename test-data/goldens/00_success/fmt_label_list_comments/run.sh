#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
mod fmt_label_list_comments;
fn access(row: Ready) { row.>select { Ready.get // access closing
} }
fn bind(row: Ready) { let .(ready // binding closing
, other) = row.>select { Ready.get; Other.get }; ready }
fn empty_access(row: Ready) { (row.>select { // empty access inside
} // empty access outside
).>select { Ready.get } }
fn empty_update(row: Ready) { (row.>with { // empty update inside
} // empty update outside
).>select { Ready.get } }
fn access_boundary(row: Ready) { row // access receiver
.>select // access opener
{ Ready.get } }
fn update_boundary(row: Ready) { row // update receiver
.>with // update opener
{ Ready.mk(()) } }
fn bind_outer(row: Ready) { let .(ready, other // binding outer close
) // binding equals
= row.>select { Ready.get; Other.get }; ready }
fn neutral_bind(row: Ready) { flow { let .(ready, other // neutral binding outer close
) = row.>select { Ready.get; Other.get }; ready } }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
for comment in 'access closing' 'binding closing' 'empty access inside' 'empty access outside' 'empty update inside' 'empty update outside' 'access receiver' 'access opener' 'update receiver' 'update opener' 'binding outer close' 'binding equals' 'neutral binding outer close'; do
  test "$(printf '%s\n' "$formatted" | grep -F -c "// $comment")" -eq 1
done
printf '%s\n' 'row-list closing comments preserved'

#!/bin/sh
# Explicit selections retain argv order, including overlapping directories;
# each directory and the no-argument walk retain deterministic path order.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
mkdir -p "$work/d/inner" "$work/.originals/d/inner"
for module in z a d/z d/a d/inner/m; do
  printf 'module %s;fn main(){()}\n' "$module" > "$work/$module.kio"
  cp "$work/$module.kio" "$work/.originals/$module.kio"
done
cd "$work"
check_dirty() {
  status=0
  "$KIO_BIN" fmt --check "$@" || status=$?
  [ "$status" -eq 60 ]
}
printf 'explicit files\n'
check_dirty z.kio a.kio z.kio
printf 'mixed selections\n'
check_dirty z.kio d a.kio z.kio d/inner/m.kio
for module in z a d/z d/a d/inner/m; do
  cmp "$module.kio" ".originals/$module.kio"
done
printf 'rewrite\n'
"$KIO_BIN" fmt z.kio d a.kio z.kio d/inner/m.kio
"$KIO_BIN" fmt --check z.kio d a.kio
for module in z a d/z d/a d/inner/m; do
  cp ".originals/$module.kio" "$module.kio"
done
printf 'implicit walk\n'
check_dirty

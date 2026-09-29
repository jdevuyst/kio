#!/bin/sh

set -eu

[ "$#" -gt 0 ] || exit 1
for file in "$@"; do
  [ -s "$file" ] || exit 1
done

awk '
  FILENAME != file {
    if (file != "" && tags != 1) bad = 1
    file = FILENAME
    tags = 0
  }
  $1 == "KIO_CI_CPU" {
    tags++
    if (NF != 3 || $2 !~ /^[0-9]+([.][0-9]+)?$/ ||
        $3 !~ /^[0-9]+([.][0-9]+)?$/) {
      bad = 1
      next
    }
    sum += $2 + $3
  }
  END {
    if (bad || file == "" || tags != 1) exit 1
    printf "%.1f\n", sum
  }
' "$@"

#!/bin/sh
set -eu
formatted=$("$KIO_BIN" fmt - <workdir/main.kio)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
case "$formatted" in
  *'__"A long compile-time message with Unicode λ and an escaped newline\nremains one complete constructor literal even after formatting."__('*) ;;
  *) exit 1 ;;
esac
case "$formatted" in *'// proof comment'*'// closing comment'*) ;; *) exit 1 ;; esac
printf '%s\n' 'compile-time literal formatting is stable'

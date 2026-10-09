#!/bin/sh
# Rejected manual arguments never echo terminal control sequences verbatim.
set -eu
mkdir -p out
escape=$(printf '\033')
reject_safely() {
  set +e
  "$KIO_BIN" manual "$@" >out/stdout 2>out/stderr
  status=$?
  set -e
  if [ "$status" != 2 ] || [ -s out/stdout ] || [ ! -s out/stderr ]; then
    printf '%s\n' 'manual argument did not produce a usage diagnostic' >&2
    exit 1
  fi
  LC_ALL=C tr -d '\033' <out/stderr >out/without-escape
  cmp -s out/stderr out/without-escape || {
    printf '%s\n' 'manual diagnostic echoed a terminal escape character' >&2
    exit 1
  }
}
reject_safely cat "docs/${escape}missing"
reject_safely cat "/${escape}missing"
reject_safely "${escape}invalid"
reject_safely "${escape}invalid" --help
reject_safely export "--format=${escape}invalid"

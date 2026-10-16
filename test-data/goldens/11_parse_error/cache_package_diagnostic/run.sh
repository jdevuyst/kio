#!/bin/sh
# Every cache operation diagnoses the malformed package before touching its
# cache. The parse category and source context do not depend on the operation.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp workdir/probe.pkg.kio "$work/probe.pkg.kio"
mkdir "$work/cache"
cp workdir/probe.pkg.kio "$work/cache/keep"
cp -R "$work/cache" "$work/cache-before"
failed=0
for command in clear path gc; do
    status=0
    (cd "$work" && "$KIO_BIN" cache "$command") >"$work/stdout" 2>"$work/stderr" || status=$?
    cat "$work/stderr" >&2
    if [ "$status" -ne 11 ]; then
        printf 'cache %s: expected parse error 11, got %s\n' "$command" "$status" >&2
        failed=1
    fi
    test ! -s "$work/stdout"
    cmp workdir/probe.pkg.kio "$work/probe.pkg.kio"
    diff -r "$work/cache-before" "$work/cache"
    grep -q 'probe.pkg.kio:' "$work/stderr" || failed=1
    grep -q 'out 17' "$work/stderr" || failed=1
    grep -q '\^' "$work/stderr" || failed=1
done
test "$failed" -eq 0
exit 11

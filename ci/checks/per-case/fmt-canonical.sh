#!/bin/sh
# ROUTING: case-binary
#
# Per-case `kio fmt` canonical-form check.
#
# Invoked once per (case, binary) by ci/run-tests.sh's --check
# pipeline (routing marker above). cwd is the original case
# directory. The runner sets:
#
#   KIO_BIN          — the kio compiler binary for this (case, binary)
#                       unit. The check exercises that binary's
#                       `kio fmt --check` codepath; impls sharing the
#                       binary share the answer.
#   KIO_TEST_UPDATE  — "1" when run-tests.sh is in -u/--update-
#                       expected mode. The check then runs
#                       `kio fmt` in place to bring source into
#                       agreement, instead of failing on a mismatch.
#
# Contract: every `*.kio` file under `workdir/` must already be in
# canonical form (`kio fmt --check` exits 0). Files whose source
# doesn't parse are out of scope and treated as n/a — they're not the
# canonicality check's concern.
#
# Opt-out: place an empty `SKIP_KIO_FMT_CHECK` marker in the case
# root. Use only when the case's *subject* is deliberately non-
# canonical input — fmt round-trip fixtures, parser-acceptance
# fixtures. Don't use it to paper over a fmt bug that breaks an
# otherwise canonicalizable case; flag the bug separately instead.
#
# POSIX sh only.

set -u

if [ -z "${KIO_BIN:-}" ]; then
  printf 'fmt-canonical: KIO_BIN is not set\n' >&2
  exit 2
fi

if [ -f SKIP_KIO_FMT_CHECK ] || [ ! -d workdir ]; then
  exit 0
fi

if [ "${KIO_TEST_UPDATE:-0}" = 1 ]; then
  output=$("$KIO_BIN" fmt workdir 2>&1)
  status=$?
  invocation='kio fmt'
else
  output=$("$KIO_BIN" fmt --check workdir 2>&1)
  status=$?
  invocation='kio fmt --check'
fi

case "$status" in
  0|11)
    exit 0
    ;;
  60)
    if [ -n "$output" ]; then
      # shellcheck disable=SC2016 # backticks are literal in the user message
      printf 'fmt-canonical: non-canonical source files (run `kio fmt workdir` to fix):\n%s\n' "$output" >&2
    else
      printf 'fmt-canonical: %s exited 60 with no dirty paths reported\n' "$invocation" >&2
    fi
    exit 1
    ;;
  *)
    printf 'fmt-canonical: %s workdir exited %s\n' "$invocation" "$status" >&2
    if [ -n "$output" ]; then
      printf '%s\n' "$output" >&2
    fi
    exit 1
    ;;
esac

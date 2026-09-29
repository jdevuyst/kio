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
  "$KIO_BIN" fmt workdir >/dev/null 2>&1 || :
  exit 0
fi

dirty=$("$KIO_BIN" fmt --check workdir 2>/dev/null)
status=$?

case "$status" in
  0)
    exit 0
    ;;
  60)
    if [ -n "$dirty" ]; then
      # shellcheck disable=SC2016 # backticks are literal in the user message
      printf 'fmt-canonical: non-canonical source files (run `kio fmt workdir` to fix):\n%s\n' "$dirty" >&2
    else
      printf 'fmt-canonical: kio fmt --check exited 60 with no dirty paths reported\n' >&2
    fi
    exit 1
    ;;
  *)
    # Parse error or other — out of scope for canonical check.
    exit 0
    ;;
esac

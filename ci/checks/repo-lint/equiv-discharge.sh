#!/bin/sh
#
# Check that every `run.sh` golden whose package declares `equiv` blocks
# actually discharges them with `kio test`.
#
# `kio test` discharges every `equiv` block in a package. The standard
# `run.args` path runs `kio test` as a build prerequisite (see
# `ci/run-tests.sh` `execute_case`), so every `run.args` case discharges
# its `equiv` blocks automatically. A `run.sh` case opts out of that path
# and owns its own tool invocations — so a `run.sh` golden that declares
# `equiv` but never runs `kio test` ships those `equiv` laws unexercised.
# This gate fails on that shape.
#
# Reachable-discharge cases only. `kio test` loads + typechecks +
# discharges, so a case expected to fail before discharge — CLI (2), a
# non-specific compile error (10), parse (11), use (12), name-resolution
# (13), type (14), elaborator (15), totality (16), bridge (20), or
# dependency (30) error — never reaches discharge and is exempt. Everything else reaches it: success (0), a
# build/emit error (40, typecheck already passed), a discharge failure
# (50, which IS a discharge), a runtime exit (90), the sig tiers. The
# exemption keys on the case's `expected.exit`, NOT its bucket directory,
# so it is robust to bucket naming and to buckets that mix outcomes
# (90_runtime_exit holds both exit-90 and exit-1 cases).
#
# The POC and castle corpora carry the same obligation through their own
# maturity checks (`audit-corpus` §§ POC 6, Castle 7), which assert a
# clean `kio test` on every case; this gate covers the goldens.
#
# Gate-enforced counterpart of the `audit-corpus` skill's § 7 and
# the `TESTING.md` § Test layers per-case-checks contract.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

# `equiv` at statement position: the keyword opening a declaration
# (`equiv <name>[...](...) { … }`), never a substring of an identifier or
# text inside a `//` comment.
EQUIV_DECL='^[[:space:]]*equiv([[:space:]]|[({]|$)'

# A `kio test` invocation on a non-comment command line. Matches the bare
# command and shell-variable references to the kio binary, any case, with
# or without wrapping quotes/braces: `kio test`, `"$KIO_BIN" test`,
# `${KIO_BIN} test`, `"$kio" test`.
KIO_TEST='(^|[^A-Za-z_])[Kk][Ii][Oo][A-Za-z_]*[}"]?[[:space:]]+test([[:space:]]|$)'

findings=$(
  find test-data/goldens -name run.sh | while IFS= read -r sh; do
    case_dir=$(dirname "$sh")

    # No `equiv` in the case's committed sources → nothing to discharge.
    grep -rlqE --include='*.kio' --exclude-dir=out --exclude-dir=.kio-cache \
      "$EQUIV_DECL" "$case_dir" 2>/dev/null || continue

    # Exempt a case expected to fail before discharge (see the header). An
    # unreadable expected.exit defaults to 0, so the gate applies — a missing
    # assertion is surfaced, not silently skipped.
    exit_code=0
    if [ -r "$case_dir/expected.exit" ]; then
      IFS= read -r exit_code <"$case_dir/expected.exit" || :
    fi
    case "$exit_code" in
      2|10|11|12|13|14|15|16|20|30) continue ;;
    esac

    # A `kio test` invocation on a non-comment line discharges the blocks.
    if ! grep -vE '^[[:space:]]*#' "$sh" | grep -qE "$KIO_TEST"; then
      printf '%s\n' "$case_dir"
    fi
  done
)

if [ -n "$findings" ]; then
  echo "equiv-discharge: run.sh goldens that declare \`equiv\` but never run" >&2
  echo "\`kio test\`, leaving those equiv laws unexercised:" >&2
  echo "$findings" | sed 's/^/  /' >&2
  echo >&2
  echo "Fix each by discharging the equiv blocks in the case's run.sh:" >&2
  echo "  \"\$KIO_BIN\" test" >&2
  exit 1
fi

echo "equiv-discharge: OK (every run.sh golden with equiv runs kio test)"

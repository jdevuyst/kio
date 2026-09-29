#!/bin/sh
# ROUTING: case-binary
#
# Per-case IS_KIO_PRIME biconditional check.
#
# Invoked once per (case, binary) by ci/run-tests.sh's --check
# pipeline (routing marker above). cwd is the original case
# directory. The runner sets:
#
#   KIO_PRIME_CHECK_BIN     — `kio-prime-check` binary (the standalone
#                              Kio' grammar verifier; impl-independent)
#   KIO_TEST_UPDATE    — "1" when run-tests.sh is in -u/--update-
#                              expected mode. The check then creates
#                              or removes IS_KIO_PRIME in cwd to bring
#                              the case into agreement, instead of
#                              failing on a mismatch.
#
# The contract is biconditional (per `specs/prime.md`): a case
# carries an IS_KIO_PRIME marker iff every regular-module `.kio` file
# under `workdir/` parses against the formal Kio' grammar. Package
# files are package-boundary syntax, not a regular-module Kio' source
# file. Root `module <name>;` files with `env { ... }` are
# regular-module Kio' files and are checked here.
#
# This check is impl-independent — it operates on the case source
# via KIO_PRIME_CHECK_BIN, not on KIO_BIN's output — so the
# case-binary routing collapses the two-per-case redundancy the
# impl-routing model used to incur.
#
# POSIX sh only.

set -eu

if [ -z "${KIO_PRIME_CHECK_BIN:-}" ]; then
  printf 'prime-marker: KIO_PRIME_CHECK_BIN is not set\n' >&2
  exit 2
fi

src_dir=workdir
marker=IS_KIO_PRIME

has_marker=0
[ -f "$marker" ] && has_marker=1

should=0
if [ -d "$src_dir" ]; then
  # Every regular-module `.kio` under `workdir/` is asserted, materialized
  # dependency trees at `workdir/<local>/…` and symlinked modules included:
  # the kio-prime differential compiles the whole materialized closure
  # through the Kio'-only binary, so the marker must hold for exactly the
  # code that run compiles. When the case directory itself holds tracked
  # content (a corpus case in the repo checkout), only tracked files count —
  # the committed closure is what a corpus run compiles, and untracked
  # residue (e.g. a partial tree left by an interrupted in-place `kio dep
  # fetch`) would poison the verdict, or under KIO_TEST_UPDATE silently
  # delete a correct marker. A case directory with no tracked content (a
  # generated case in a scratch dir — even one under a repo-internal temp
  # path — or a not-yet-added authoring draft) is asserted wholesale.
  # `*.pkg.kio` is package-boundary syntax, not a regular module, and
  # `out/` is build output; both are excluded.
  discovery=$(mktemp -d)
  trap 'rm -rf "$discovery"' EXIT INT TERM HUP
  candidates=$discovery/candidates
  tracked=$discovery/tracked
  files=$discovery/files
  : > "$tracked"
  tracked_only=0
  if [ "$(git rev-parse --is-inside-work-tree 2>/dev/null)" = true ]; then
    # One index query serves both case ownership and literal membership.
    # A generated draft inside a checkout still has no tracked case content.
    if ! git -c core.quotePath=true ls-files -- . > "$tracked"; then
      printf 'prime-marker: cannot list tracked case files\n' >&2
      exit 1
    fi
    [ ! -s "$tracked" ] || tracked_only=1
  fi
  # Check traversal before consuming its output: a partial walk is not a
  # smaller valid source set and must never change a marker.
  if ! find -L "$src_dir" -type f -name '*.kio' \
    ! -name '*.pkg.kio' ! -path "$src_dir/out/*" > "$files"; then
    printf 'prime-marker: cannot discover source files\n' >&2
    exit 1
  fi

  quoted=0
  grep '^"' "$tracked" >/dev/null || quoted=$?
  case "$quoted" in
    0)
      # Git C-quotes non-ASCII and escaped paths. Literal queries leave both
      # pathname decoding and filesystem Unicode normalization to Git.
      eligible=$discovery/eligible
      : > "$eligible"
      while IFS= read -r f; do
        lookup_status=0
        git ls-files --error-unmatch -- ":(literal)$f" \
          >/dev/null 2> "$discovery/git-error" || lookup_status=$?
        case "$lookup_status" in
          0) printf '%s\n' "$f" >> "$eligible" ;;
          1) ;; # This exact path is untracked.
          *)
            cat "$discovery/git-error" >&2
            printf 'prime-marker: cannot check tracked source %s\n' "$f" >&2
            exit 1
            ;;
        esac
      done < "$files"
      files=$eligible
      tracked_only=0
      ;;
    1) ;;
    *)
      printf 'prime-marker: cannot inspect tracked paths\n' >&2
      exit 1
      ;;
  esac

  # Keep find order and the caller's character classes. This is a
  # first-meaningful-token filter, not a Kio parser; the verifier below remains
  # responsible for every selected module and retains its early-stop order.
  if ! awk -v tracked_only="$tracked_only" '
      BEGIN {
        tracked_file = ARGV[1]; read_error = ARGV[2]
        delete ARGV[1]; delete ARGV[2]
        while ((status = (getline path < tracked_file)) > 0) tracked[path] = 1
        close(tracked_file)
        if (status < 0) { print tracked_file > read_error; exit 1 }
      }
      {
        path = $0
        if (tracked_only && !(path in tracked)) next
        while ((status = (getline line < path)) > 0) {
          sub(/^[[:space:]]*/, "", line)
          if (line == "" || line ~ /^\/\//) continue
          if (line ~ /^module([[:space:]]|\/\/|$)/) print path
          break
        }
        close(path)
        if (status < 0) { print path > read_error; exit 1 }
      }
    ' "$tracked" "$discovery/read-error" < "$files" > "$candidates"; then
    if [ -s "$discovery/read-error" ]; then
      printf 'prime-marker: cannot read source inventory or module: %s\n' \
        "$(cat "$discovery/read-error")" >&2
    else
      printf 'prime-marker: cannot classify source files\n' >&2
    fi
    exit 1
  fi

  reg_count=$(wc -l < "$candidates" | tr -d ' ')
  if [ "$reg_count" -gt 0 ]; then
    should=1
    while IFS= read -r f; do
      if ! "$KIO_PRIME_CHECK_BIN" "$f" >/dev/null 2>&1; then
        should=0
        break
      fi
    done < "$candidates"
  fi
fi

if [ "$has_marker" = "$should" ]; then
  exit 0
fi

if [ "${KIO_TEST_UPDATE:-0}" = 1 ]; then
  if [ "$should" = 1 ]; then
    : > "$marker"
    printf 'prime-marker: created IS_KIO_PRIME (case is Kio'"'"')\n'
  else
    rm -f "$marker"
    printf 'prime-marker: removed IS_KIO_PRIME (case is not Kio'"'"')\n'
  fi
  exit 0
fi

if [ "$has_marker" = 1 ]; then
  if [ "${reg_count:-0}" -eq 0 ]; then
    printf 'prime-marker: IS_KIO_PRIME present but no regular-module *.kio files are asserted under workdir/\n' >&2
  else
    printf 'prime-marker: IS_KIO_PRIME present but at least one regular-module *.kio file does not parse as Kio'"'"'\n' >&2
  fi
else
  printf 'prime-marker: missing IS_KIO_PRIME — every regular-module *.kio file parses as Kio'"'"'\n' >&2
fi
exit 1

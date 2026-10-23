#!/bin/sh
#
# Reject symlinks and byte-identical copies of the canonical macro library.
# Consumers declare a path dependency and commit its re-rooted materialization;
# test-data/README.md documents that ownership contract. This check complements
# dep-materialization.sh, which verifies the generated dependency trees.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

CANON_DIR="test-data/poc/macros/workdir"
NAMES="macro_util.kio algebraic_macros.kio spine_macros.kio match.kio derive.kio"

# The dynamic-loader infrastructure has a separate source-assembly contract,
# checked by its builder and publication self-test.
EXEMPT_PREFIX="test-data/poc/dyn_load_prime/"

# (a) git-tracked symlinks into the canonical macros workdir. Mode 120000
# in `git ls-files -s` marks a symlink; check each target.
symlinks=$(
  git ls-files -s test-data | while read -r mode _hash _stage path; do
    [ "$mode" = "120000" ] || continue
    case "$path" in "$CANON_DIR"/*) continue ;; esac
    case "$path" in "$EXEMPT_PREFIX"*) continue ;; esac
    case "$(readlink "$path")" in
      *macros/workdir*) printf '%s\n' "$path" ;;
    esac
  done
)

# (b) git-tracked byte-identical copies of a canonical reference module.
# Committed materialized dependency trees match the `$name` globs (their
# re-rooted `<consumer>/macros/<name>` lives under test-data too) but are
# re-rooted derivatives, so the `cmp` below finds them different — the
# normal, no-finding path.
#
# The byte test uses `if cmp …; then …; fi`, not `cmp … && printf …`.
# Under a POSIX `/bin/sh` (dash) with `set -e`, a bare `cmd && action`
# whose `cmd` fails — the usual case here, since most matched paths are
# re-rooted and differ — as the *last* statement of the `while`-pipe
# subshell aborts the enclosing `for` mid-iteration, dropping any finding
# from a later `$name`. The `if` form makes the expected `cmp` failure a
# tested condition, which `set -e` does not act on.
copies=$(
  for name in $NAMES; do
    canon="$CANON_DIR/$name"
    [ -f "$canon" ] || continue
    git ls-files "test-data/*/$name" "test-data/*/*/$name" "test-data/*/*/*/$name" \
                 "test-data/*/*/*/*/$name" "test-data/*/*/*/*/*/$name" 2>/dev/null \
      | while read -r path; do
          case "$path" in "$CANON_DIR"/*) continue ;; esac
          case "$path" in "$EXEMPT_PREFIX"*) continue ;; esac
          # Symlinks are reported by the (a) check above; a real-file
          # byte-identical copy is the (b) violation. Skip symlinks so
          # `cmp` (which follows them) does not double-report.
          [ -L "$path" ] && continue
          [ -f "$path" ] || continue
          if cmp -s "$path" "$canon"; then printf '%s\n' "$path"; fi
        done
  done
)

status=0

if [ -n "$symlinks" ]; then
  echo "golden-reference-deps: git-tracked symlinks into the canonical macros" >&2
  echo "library found (consume macros through a \`dependency macros;\` instead):" >&2
  echo "$symlinks" | sed 's/^/  /' >&2
  echo >&2
  status=1
fi

if [ -n "$copies" ]; then
  echo "golden-reference-deps: byte-identical copies of canonical macros" >&2
  echo "reference modules found (consume macros through a \`dependency macros;\`" >&2
  echo "instead of copying its modules):" >&2
  echo "$copies" | sed 's/^/  /' >&2
  echo >&2
  status=1
fi

if [ "$status" -ne 0 ]; then
  echo "Fix each by depending on macros and importing the re-rooted module:" >&2
  echo "  // <local>.dep.kio at the consumer package root" >&2
  echo "  dependency macros;" >&2
  echo "  source { path \"<rel>/macros.pkg.kio\"; }" >&2
  echo "  // then, in a module: import macros/derive(macro derive);  import macros/match(macro match);" >&2
  exit 1
fi

echo "golden-reference-deps: OK (macros consumed only via dependencies)"

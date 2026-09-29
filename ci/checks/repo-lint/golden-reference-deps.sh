#!/bin/sh
#
# Check that test cases consume the shared POC elaborator library
# through a `dependency elab;` path dependency — never by symlinking or
# byte-copying its modules into their own package.
#
# The canonical library lives under:
#
#   test-data/poc/elab/workdir/   (package elab)
#
# A consumer that needs the elaborators declares a dependency:
#
#   // elab.dep.kio
#   dependency elab;
#   source { path "<rel>/elab.pkg.kio"; }
#
# and imports `use elab/derive …;`, `use elab/match …;`, etc. Depending
# on elab materializes its modules under an `elab/` tree at the consumer
# package root; that re-rooted tree is committed (the consumer ships its
# dependency's materialized closure). The materialized modules are
# re-rooted derivatives (`module derive;` becomes `module elab/derive;`),
# not byte copies of the canonical source — so this gate, which forbids
# byte-identical canonical copies, never trips on a committed materialized
# tree. (ci/checks/repo-lint/dep-materialization.sh checks the committed
# tree itself.)
#
# This gate fails on the two pre-dependency workarounds:
#
#   (a) a git-tracked symlink pointing into test-data/poc/elab/workdir/
#       (the old reuse mechanism — now replaced by dependencies), and
#   (b) a git-tracked byte-identical real copy of a canonical reference
#       module (a maintenance hazard: copies silently drift).
#
# Both must be zero: a test case reaches elab's elaborators only through
# a dependency. (This is the inverted successor of the former
# `golden-reference-symlinks` gate, which *mandated* symlinks; the
# cross-package dependency migration flipped it. It is the gate-enforced
# counterpart of the audit-corpus skill's § 5.)
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

CANON_DIR="test-data/poc/elab/workdir"
NAMES="elaborator_util.kio algebraic_elaborators.kio spine_elaborators.kio match.kio derive.kio"

# One documented exemption: `dyn_load_prime` is not a normal package that a
# host depends on — it is test infrastructure (the dyn-load-prime interpreter, and
# the dyn-load reference) assembled into a host by custom scripts
# (`ci/infra/kio-test-runner-rs/dyn-load-prime-driver/build-driver.sh` and
# `test-data/goldens/00_success/exec_dyn_load_integration/run.sh`), which `cp`
# the package's modules into an assembly dir. It reuses elab's elaborator
# modules via LIVE symlinks (tracking the canonical source with zero drift),
# not as a `dependency elab;` — making it a real dependency would force those
# assembly scripts to materialize a dependency, and a byte-copy would go stale.
# So its symlinks are intentional and exempt here.
EXEMPT_PREFIX="test-data/poc/dyn_load_prime/"

# (a) git-tracked symlinks into the canonical elab workdir. Mode 120000
# in `git ls-files -s` marks a symlink; check each target.
symlinks=$(
  git ls-files -s test-data | while read -r mode _hash _stage path; do
    [ "$mode" = "120000" ] || continue
    case "$path" in "$CANON_DIR"/*) continue ;; esac
    case "$path" in "$EXEMPT_PREFIX"*) continue ;; esac
    case "$(readlink "$path")" in
      *elab/workdir*) printf '%s\n' "$path" ;;
    esac
  done
)

# (b) git-tracked byte-identical copies of a canonical reference module.
# Committed materialized dependency trees match the `$name` globs (their
# re-rooted `<consumer>/elab/<name>` lives under test-data too) but are
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
  echo "golden-reference-deps: git-tracked symlinks into the canonical elab" >&2
  echo "library found (consume elab through a \`dependency elab;\` instead):" >&2
  echo "$symlinks" | sed 's/^/  /' >&2
  echo >&2
  status=1
fi

if [ -n "$copies" ]; then
  echo "golden-reference-deps: byte-identical copies of canonical elab" >&2
  echo "reference modules found (consume elab through a \`dependency elab;\`" >&2
  echo "instead of copying its modules):" >&2
  echo "$copies" | sed 's/^/  /' >&2
  echo >&2
  status=1
fi

if [ "$status" -ne 0 ]; then
  echo "Fix each by depending on elab and importing the re-rooted module:" >&2
  echo "  // <local>.dep.kio at the consumer package root" >&2
  echo "  dependency elab;" >&2
  echo "  source { path \"<rel>/elab.pkg.kio\"; }" >&2
  echo "  // then, in a module: use elab/derive …;  use elab/match …;" >&2
  exit 1
fi

echo "golden-reference-deps: OK (elab consumed only via dependencies)"

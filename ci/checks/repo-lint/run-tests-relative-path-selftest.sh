#!/bin/sh

# Keep path-bearing --impl-def executables anchored to the harness invocation
# directory after workers change into individual case directories.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
RUN_TESTS_SH=${RUN_TESTS_SH:-"$REPO_ROOT/ci/run-tests.sh"}

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

mkdir -p "$scratch/tools" "$scratch/cases/relative-path" "$scratch/tmp" \
  "$scratch/cache"

# Exercise native discovery spellings without claiming Windows execution on
# Unix. The production resolver owns all classification; only the shell's
# command discovery and Windows path converter are replaced here.
awk '/^resolve_impl_executable\(\) \{/ { copy = 1 }
     copy { print }
     copy && /^\}/ { exit }' "$RUN_TESTS_SH" >"$scratch/resolve.sh"
(
  # shellcheck disable=SC1090,SC1091
  . "$scratch/resolve.sh"
  # shellcheck disable=SC2034 # consumed by the extracted production function
  INVOCATION_DIR=$scratch
  # shellcheck disable=SC2034 # consumed by the extracted production function
  path_probe_dir=$scratch/tools
  # shellcheck disable=SC2317 # called by the extracted production function
  command() {
    [ "$#" -eq 2 ] && [ "$1" = -v ] && [ "$2" = fixture ] || return 2
    [ "$discovered" != missing ] || return 1
    printf '%s\n' "$discovered"
  }
  # shellcheck disable=SC2317 # called by the extracted production function
  cygpath() {
    [ "$#" -eq 2 ] && [ "$1" = -u ] && [ "$2" = "$discovered" ] || return 2
    printf 'converted\n' >>"$scratch/conversions"
    [ "$converted" != failure ] || return 1
    printf '%s\n' "$converted"
  }
  expect_resolution() {
    discovered=$1
    expected=$2
    converted=$3
    : >"$scratch/conversions"
    actual=$(resolve_impl_executable fixture) || exit 1
    if [ "$actual" != "$expected" ]; then
      printf 'run-tests-relative-path-selftest: resolved <%s> to <%s>, expected <%s>\n' \
        "$discovered" "$actual" "$expected" >&2
      exit 1
    fi
    if [ -n "$converted" ]; then
      [ "$(cat "$scratch/conversions")" = converted ] || exit 1
    else
      [ ! -s "$scratch/conversions" ] || exit 1
    fi
  }
  OS=Windows_NT
  MSYSTEM=
  export OS MSYSTEM
  expect_resolution 'D:/Tool Dir/kio.exe' '/d/Tool Dir/kio.exe' '/d/Tool Dir/kio.exe'
  expect_resolution 'e:\Tool Dir\runner.exe' '/e/Tool Dir/runner.exe' '/e/Tool Dir/runner.exe'
  expect_resolution '\\server\share\kio.exe' '//server/share/kio.exe' '//server/share/kio.exe'
  expect_resolution '//server/share/kio.exe' '//server/share/kio.exe' ''
  expect_resolution '/d/Tool Dir/kio.exe' '/d/Tool Dir/kio.exe' ''
  expect_resolution 'tools/kio' "$scratch/tools/kio" ''
  expect_resolution 'true' 'true' ''
  OS=
  MSYSTEM=MINGW64
  expect_resolution 'D:/kio.exe' '/d/kio.exe' '/d/kio.exe'
  discovered='D:/kio.exe'
  converted=failure
  if resolve_impl_executable fixture >/dev/null 2>&1; then
    printf 'run-tests-relative-path-selftest: failed native conversion was accepted\n' >&2
    exit 1
  fi
  MSYSTEM=
  expect_resolution 'D:/kio.exe' "$scratch/D:/kio.exe" ''
  discovered=missing
  if resolve_impl_executable fixture >/dev/null 2>&1; then
    printf 'run-tests-relative-path-selftest: missing executable was accepted\n' >&2
    exit 1
  fi
)

cat >"$scratch/tools/kio" <<'EOF'
#!/bin/sh
case "${1:-} ${2:-}" in
  'cache --help'|'cache clear') exit 0 ;;
esac
printf 'kio\n'
EOF

cat >"$scratch/tools/runner" <<'EOF'
#!/bin/sh
printf 'runner\n'
EOF

cat >"$scratch/cases/relative-path/run.sh" <<'EOF'
#!/bin/sh
set -eu
"$KIO_BIN"
"$KIO_RUNNER"
EOF

printf '0\n' >"$scratch/cases/relative-path/expected.exit"
printf 'kio\nrunner\n' >"$scratch/cases/relative-path/expected.stdout"
: >"$scratch/cases/relative-path/expected.stderr.ignore"
chmod +x "$scratch/tools/kio" "$scratch/tools/runner" \
  "$scratch/cases/relative-path/run.sh"

if ! (
  cd "$scratch"
  KIO_CI_SCHEDULE=DISABLE TMPDIR="$scratch/tmp" sh "$RUN_TESTS_SH" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def='name=relative,kio=./tools/kio,runner=./tools/runner,target=js' \
    --jobs=1 relative-path
) >"$scratch/run.log" 2>&1; then
  cat "$scratch/run.log" >&2
  printf 'run-tests-relative-path-selftest: relative impl executable failed after case-directory change\n' >&2
  exit 1
fi

cp "$scratch/tools/kio" "$scratch/kio"
cp "$scratch/tools/runner" "$scratch/runner"

if ! (
  cd "$scratch"
  PATH=:"$PATH" KIO_CI_SCHEDULE=DISABLE TMPDIR="$scratch/tmp" \
    sh "$RUN_TESTS_SH" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def='name=empty-path,kio=kio,runner=runner,target=js' \
    --jobs=1 relative-path
) >"$scratch/empty-path.log" 2>&1; then
  cat "$scratch/empty-path.log" >&2
  printf 'run-tests-relative-path-selftest: empty PATH component executable failed after case-directory change\n' >&2
  exit 1
fi

# A bare shell builtin also has a slashless `command -v` result, but it is
# not a cwd-relative filesystem hit and must remain bare.
printf 'kio\n' >"$scratch/cases/relative-path/expected.stdout"
if ! (
  cd "$scratch"
  PATH=:"$PATH" KIO_CI_SCHEDULE=DISABLE TMPDIR="$scratch/tmp" \
    sh "$RUN_TESTS_SH" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def='name=builtin,kio=kio,runner=true,target=js' \
    --jobs=1 relative-path
) >"$scratch/builtin.log" 2>&1; then
  cat "$scratch/builtin.log" >&2
  printf 'run-tests-relative-path-selftest: slashless shell builtin was mistaken for a relative executable\n' >&2
  exit 1
fi

printf 'run-tests-relative-path-selftest: ok\n'

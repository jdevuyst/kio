#!/bin/sh

# Pin the regular-target + Kio' batching path and its fail-closed legacy
# fallback without invoking Kio or a host compiler.

set -eu

TAB=$(printf '\t')
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
ROUNDTRIP=${ROUNDTRIP:-"$REPO_ROOT/ci/checks/per-case/kio-prime-roundtrip.sh"}

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/kio-prime-roundtrip-batching.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
mkdir -p "$scratch/bin" "$scratch/cases" "$scratch/tmp"
command_log=$scratch/commands.log
: >"$command_log"

cat >"$scratch/bin/kio-full" <<'EOF'
#!/bin/sh
set -eu

mode=ok
[ ! -f FIXTURE_MODE ] || IFS= read -r mode <FIXTURE_MODE
log_command() {
  printf '%s\t%s\t%s\t%s\t%s\n' "$KIO_TEST_RUN_LABEL" "$1" "$mode" \
    "${2:--}" "$PWD" >>"$KIO_TEST_COMMAND_LOG"
}
write_artifact() {
  out=$1 payload=$2
  mkdir -p "$out/nested"
  printf '%s\n' "$payload" >"$out/artifact.txt"
  printf 'nested\000bytes\n' >"$out/nested/bytes.bin"
}
write_prime() {
  mkdir -p __kio_roundtrip_prime
  cp ./*.pkg.kio FIXTURE_MODE __kio_roundtrip_prime/
  printf 'module main;\n' >__kio_roundtrip_prime/main.kio
}

if [ "${1:-}" = debug ] &&
   [ "${2:-}" = kio-prime-roundtrip-package ]; then
  [ "$#" -eq 4 ] || exit 77
  pkg_path=$3
  selected_target=$4
  [ ! -f "${pkg_path%/*}/FIXTURE_MODE" ] ||
    IFS= read -r mode <"${pkg_path%/*}/FIXTURE_MODE"
  log_command augment "$selected_target"
  case "$mode" in
    package-internal-match)
      printf 'synthetic package read error\n' >&2
      exit 1
      ;;
    package-parse-*)
      printf 'synthetic package parse error\n' >&2
      exit 11
      ;;
  esac
  [ "$mode" != inapplicable ] || exit 0
  package_name=$(sed -n 's/^package \([^;]*\);$/\1/p' "$pkg_path")
  [ -n "$package_name" ] || exit 78
  printf 'package %s;\n\nbuild {\n  cache ();\n\n  target %s {\n' \
    "$package_name" "$selected_target"
  if grep -Fq '// }' "$pkg_path"; then
    printf '    // }\n'
  fi
  printf '    out "__kio_roundtrip_target/";\n  };\n\n'
  printf '  target python {\n    out "original/python/";\n  };\n\n'
  printf '  target kio-prime {\n    out "__kio_roundtrip_prime/";\n  }\n}\n'
  exit 0
fi

no_cache=0
if [ "${1:-}" = --no-cache ]; then no_cache=1; shift; fi
[ "${1:-}" = build ] || exit 75
shift
target=${1:-}
log_command build-attempt "$target"
shift || :
set -- ./*.pkg.kio "$@"
pkg_path=$1
shift
pkg_selector=./$(basename "$pkg_path")
if ! grep -Fq 'target ts {' "$pkg_path" ||
   ! grep -Fq 'out "__kio_roundtrip_target/";' "$pkg_path" ||
   ! grep -Fq 'target python {' "$pkg_path" ||
   ! grep -Fq 'out "original/python/";' "$pkg_path" ||
   ! grep -Fq 'target kio-prime {' "$pkg_path" ||
   ! grep -Fq 'out "__kio_roundtrip_prime/";' "$pkg_path"; then
  printf 'roundtrip rewrote the wrong target output\n' >&2
  exit 70
fi
if ! awk '
  $1 == "target" && $2 == "ts" && $3 == "{" { in_selected = 1; next }
  in_selected && $1 == "};" { in_selected = 0; selected_closed = 1; next }
  $1 == "target" && $2 == "kio-prime" && $3 == "{" {
    prime_found = 1
    if (!selected_closed) invalid = 1
  }
  END { exit(prime_found && selected_closed && !invalid ? 0 : 1) }
' "$pkg_path"; then
  printf 'roundtrip produced a nested or missing kio-prime target\n' >&2
  exit 70
fi
if [ "$no_cache" = 1 ] && [ "${1:-}" = kio-prime ]; then
  [ "$#" -eq 2 ] && [ "$1" = kio-prime ] && [ "$2" = "$pkg_selector" ] || exit 69
  log_command combined "$target"
  case "$mode" in
    ok|mismatch|verifier-fail|combined-missing-direct|combined-missing-prime)
      [ "$mode" = combined-missing-direct ] || \
        write_artifact __kio_roundtrip_target verified
      [ "$mode" = combined-missing-prime ] || write_prime
      exit 0
      ;;
    *)
      mkdir -p __kio_roundtrip_target __kio_roundtrip_prime
      printf 'partial\n' >__kio_roundtrip_target/partial
      printf 'partial\n' >__kio_roundtrip_prime/partial
      printf 'combined sentinel\n' >&2
      exit 71
      ;;
  esac
fi
if [ "$no_cache" = 1 ] && [ "$target" != kio-prime ]; then
  [ "$#" -eq 1 ] && [ "$1" = "$pkg_selector" ] || exit 68
  log_command direct "$target"
  if [ -e __kio_roundtrip_target ] || [ -e __kio_roundtrip_prime ]; then
    printf 'partial combined output survived fallback cleanup\n' >&2
    exit 72
  fi
  [ "$mode" != direct-fail ] || exit 73
  write_artifact __kio_roundtrip_target verified
  exit 0
fi
if [ "$no_cache" = 1 ] && [ "$target" = kio-prime ]; then
  [ "$#" -eq 1 ] && [ "$1" = "$pkg_selector" ] || exit 67
  log_command prime "$target"
  [ "$mode" != prime-fail ] || exit 74
  [ "$mode" = missing-prime-output ] || write_prime
  exit 0
fi
exit 76
EOF

cat >"$scratch/bin/kio-prime" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = --no-cache ] && shift
[ "${1:-}" = build ] || exit 81
shift
target=$1
mode=ok
[ ! -f FIXTURE_MODE ] || IFS= read -r mode <FIXTURE_MODE
printf '%s\treduced\t%s\t%s\t%s\n' "$KIO_TEST_RUN_LABEL" "$mode" "$target" \
  "$PWD" >>"$KIO_TEST_COMMAND_LOG"
mkdir -p __kio_roundtrip_target/nested
if [ "$mode" = mismatch ]; then payload=mismatched; else payload=verified; fi
printf '%s\n' "$payload" >__kio_roundtrip_target/artifact.txt
printf 'nested\000bytes\n' >__kio_roundtrip_target/nested/bytes.bin
EOF

cat >"$scratch/bin/prime-check" <<'EOF'
#!/bin/sh
set -eu
printf '%s\tverifier\t%s\t-\t%s\n' "$KIO_TEST_RUN_LABEL" "$KIO_TEST_MODE" \
  "$1" >>"$KIO_TEST_COMMAND_LOG"
[ "$KIO_TEST_MODE" != verifier-fail ]
EOF

chmod +x "$scratch/bin/kio-full" "$scratch/bin/kio-prime" \
  "$scratch/bin/prime-check"

make_case() {
  name=$1 mode=$2 expected_exit=$3 hazard=${4:-}
  manifest_target=ts
  [ "$hazard" != missing-target ] || manifest_target=js
  case_dir=$scratch/cases/$name
  mkdir -p "$case_dir/workdir"
  printf '%s\n' "$mode" >"$case_dir/workdir/FIXTURE_MODE"
  cat >"$case_dir/workdir/$name.pkg.kio" <<EOF
package $name;

build {
  target $manifest_target {
EOF
  if [ "$hazard" = brace-comment ]; then
    printf '    // }\n' >>"$case_dir/workdir/$name.pkg.kio"
  fi
  cat >>"$case_dir/workdir/$name.pkg.kio" <<EOF
    out "out/ts/";
  };
  target python {
    out "original/python/";
  }
}
EOF
  printf '%s\n' "$expected_exit" >"$case_dir/expected.exit"
}

make_case ok ok 0
make_case combined-regression combined-regression 0
make_case direct-fail direct-fail 0
make_case prime-fail prime-fail 0
make_case mismatch mismatch 0
make_case verifier-fail verifier-fail 0
make_case missing-prime-output missing-prime-output 0
make_case combined-missing-direct combined-missing-direct 0
make_case combined-missing-prime combined-missing-prime 0
make_case nonzero nonzero 14
make_case brace-comment ok 0 brace-comment
make_case inapplicable inapplicable 0 missing-target
make_case package-parse-match package-parse-match 11
make_case package-parse-expected-success package-parse-expected-success 0
make_case package-parse-mismatch package-parse-mismatch 12
make_case package-internal-match package-internal-match 1

run_case() {
  label=$1 expected_status=$2
  log=$scratch/$label.log
  status=0
  (
    cd "$scratch/cases/$label"
    TMPDIR="$scratch/tmp" KIO_BIN="$scratch/bin/kio-full" \
      KIO_PRIME_BIN="$scratch/bin/kio-prime" \
      KIO_PRIME_CHECK_BIN="$scratch/bin/prime-check" KIO_TARGET=ts \
      KIO_TEST_COMMAND_LOG="$command_log" KIO_TEST_RUN_LABEL="$label" \
      KIO_TEST_MODE=$(cat workdir/FIXTURE_MODE) sh "$ROUNDTRIP"
  ) >"$log" 2>&1 || status=$?
  if [ "$status" -ne "$expected_status" ]; then
    cat "$log" >&2
    printf 'kio-prime-roundtrip-batching-selftest: %s exited %s, expected %s\n' \
      "$label" "$status" "$expected_status" >&2
    exit 1
  fi
}

event_count() {
  awk -F "$TAB" -v label="$1" -v event="$2" \
    '$1 == label && $2 == event { count++ } END { print count + 0 }' \
    "$command_log"
}
assert_count() {
  actual=$(event_count "$1" "$2")
  if [ "$actual" -ne "$3" ]; then
    printf 'kio-prime-roundtrip-batching-selftest: %s expected %s %s events, got %s\n' \
      "$1" "$3" "$2" "$actual" >&2
    exit 1
  fi
}
assert_failure() {
  if ! grep -Eq "$2" "$scratch/$1.log"; then
    cat "$scratch/$1.log" >&2
    printf 'kio-prime-roundtrip-batching-selftest: %s lost failure attribution\n' \
      "$1" >&2
    exit 1
  fi
}

run_case ok 0
assert_count ok build-attempt 1
assert_count ok combined 1
assert_count ok direct 0
assert_count ok prime 0
assert_count ok verifier 1
assert_count ok reduced 1

run_case combined-regression 1
assert_count combined-regression combined 1
assert_count combined-regression direct 1
assert_count combined-regression prime 1
assert_count combined-regression verifier 0
assert_count combined-regression reduced 0
assert_failure combined-regression 'multi-target.*regression'

run_case direct-fail 0
assert_count direct-fail combined 1
assert_count direct-fail direct 1
assert_count direct-fail prime 0
assert_count direct-fail reduced 0

run_case prime-fail 1
assert_count prime-fail combined 1
assert_count prime-fail direct 1
assert_count prime-fail prime 1
assert_count prime-fail reduced 0
assert_failure prime-fail 'full compiler failed to emit Kio'

run_case mismatch 1
assert_count mismatch combined 1
assert_count mismatch direct 0
assert_count mismatch prime 0
assert_count mismatch reduced 1
assert_failure mismatch 'artifact bytes differ'

run_case verifier-fail 1
assert_count verifier-fail combined 1
assert_count verifier-fail verifier 2
assert_count verifier-fail reduced 0
assert_failure verifier-fail 'emitted file does not parse as Kio'

run_case missing-prime-output 1
assert_count missing-prime-output combined 1
assert_count missing-prime-output direct 1
assert_count missing-prime-output prime 1
assert_count missing-prime-output verifier 0
assert_count missing-prime-output reduced 0
assert_failure missing-prime-output 'Kio build succeeded but .*__kio_roundtrip_prime was not created'

run_case combined-missing-direct 1
assert_count combined-missing-direct combined 1
assert_count combined-missing-direct direct 0
assert_count combined-missing-direct prime 0
assert_count combined-missing-direct verifier 0
assert_count combined-missing-direct reduced 0
assert_failure combined-missing-direct 'direct build succeeded but .*__kio_roundtrip_target was not created'

run_case combined-missing-prime 1
assert_count combined-missing-prime combined 1
assert_count combined-missing-prime direct 0
assert_count combined-missing-prime prime 0
assert_count combined-missing-prime verifier 0
assert_count combined-missing-prime reduced 0
assert_failure combined-missing-prime 'Kio build succeeded but .*__kio_roundtrip_prime was not created'

run_case nonzero 0
assert_count nonzero combined 0
assert_count nonzero direct 1
assert_count nonzero prime 1
assert_count nonzero verifier 1
assert_count nonzero reduced 1

run_case brace-comment 0
assert_count brace-comment build-attempt 1
assert_count brace-comment combined 1
assert_count brace-comment direct 0
assert_count brace-comment prime 0
assert_count brace-comment augment 1
assert_count brace-comment verifier 1
assert_count brace-comment reduced 1

run_case inapplicable 0
assert_count inapplicable augment 1
assert_count inapplicable build-attempt 0
assert_count inapplicable combined 0
assert_count inapplicable direct 0
assert_count inapplicable prime 0
assert_count inapplicable verifier 0
assert_count inapplicable reduced 0

run_case package-parse-match 0
assert_count package-parse-match augment 1
assert_count package-parse-match build-attempt 0
assert_failure package-parse-match 'synthetic package parse error'

run_case package-parse-expected-success 1
assert_count package-parse-expected-success augment 1
assert_count package-parse-expected-success build-attempt 0
assert_failure package-parse-expected-success 'cannot prepare package manifest'

run_case package-parse-mismatch 1
assert_count package-parse-mismatch augment 1
assert_count package-parse-mismatch build-attempt 0
assert_failure package-parse-mismatch 'cannot prepare package manifest'

run_case package-internal-match 1
assert_count package-internal-match augment 1
assert_count package-internal-match build-attempt 0
assert_failure package-internal-match 'cannot prepare package manifest'

printf 'kio-prime-roundtrip-batching-selftest: ok\n'

#!/bin/sh
#
# Prove that rebuilding the shared dyn-load-prime driver cannot invalidate an
# artifact path already returned to another golden orchestrator.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
BUILD_DRIVER=${BUILD_DRIVER:-"$REPO_ROOT/ci/infra/kio-test-runner-rs/dyn-load-prime-driver/build-driver.sh"}

tmp_base=${KIO_TMP_DIR:-${TMPDIR:-/tmp}}
mkdir -p "$tmp_base"
scratch=$(mktemp -d "$tmp_base/dyn-load-prime-driver-publication-selftest.XXXXXX") || {
  printf 'dyn-load-prime-driver-publication-selftest: cannot make scratch dir\n' >&2
  exit 2
}
child=
release="$scratch/release"
same_a=
same_b=
same_release="$scratch/same-release"
cleanup() {
  : >"$release"
  : >"$same_release"
  for pid in $child $same_a $same_b; do
    [ -n "$pid" ] || continue
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

wait_for_pause() {
  marker=$1
  pid=$2
  label=$3
  err=$4
  i=0
  while [ ! -f "$marker" ] && kill -0 "$pid" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -ge 500 ]; then
      printf 'dyn-load-prime-driver-publication-selftest: %s did not reach its controlled pause\n' "$label" >&2
      return 1
    fi
    sleep 0.01
  done
  if [ ! -f "$marker" ]; then
    wait "$pid" || true
    cat "$err" >&2
    printf 'dyn-load-prime-driver-publication-selftest: %s exited before its controlled pause\n' "$label" >&2
    return 1
  fi
}

wait_for_success() {
  pid=$1
  label=$2
  err=$3
  if ! wait "$pid"; then
    cat "$err" >&2
    printf 'dyn-load-prime-driver-publication-selftest: %s failed\n' "$label" >&2
    return 1
  fi
}

src="$scratch/dyn-load-prime"
assembly="$scratch/shared-assembly"
mkdir -p "$src/list" "$src/loader" "$src/elab" "$scratch/bin"
KIO_CI_SCHEDULE_DIR="$scratch/state"
KIO_CI_SCHEDULE_HELD=
KIO_CI_SCHEDULE_COMPILER_JOBS=2
RUSTC_WRAPPER=
RUSTC_WORKSPACE_WRAPPER=
CARGO_BUILD_RUSTC_WRAPPER=
CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=
KIO_TEST_RUNNER_COMPILER_WRAPPER=
export KIO_CI_SCHEDULE_DIR KIO_CI_SCHEDULE_HELD
export KIO_CI_SCHEDULE_COMPILER_JOBS
export RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
export CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER
export KIO_TEST_RUNNER_COMPILER_WRAPPER
printf 'module core;\n' >"$src/core.kio"
printf 'module list.item;\n' >"$src/list/item.kio"
printf 'module loader.item;\n' >"$src/loader/item.kio"
printf 'module elab.item;\n' >"$src/elab/item.kio"

cat >"$scratch/bin/kio-a" <<'EOF'
#!/bin/sh
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 90 ;;
esac
[ "${DYN_DRIVER_FAIL_IF_CALLED:-}" != 1 ] || exit 91
mkdir -p out/js
printf 'first artifact\n' >out/js/dyn_load_prime_driver.js
EOF
cat >"$scratch/bin/kio-b" <<'EOF'
#!/bin/sh
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 90 ;;
esac
: >"$DYN_DRIVER_ENTERED"
while [ ! -f "$DYN_DRIVER_RELEASE" ]; do
  sleep 0.01
done
mkdir -p out/js
printf 'second artifact\n' >out/js/dyn_load_prime_driver.js
EOF
cat >"$scratch/bin/kio-same" <<'EOF'
#!/bin/sh
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 90 ;;
esac
: >"$DYN_DRIVER_READY"
while [ ! -f "$DYN_DRIVER_RELEASE" ]; do
  sleep 0.01
done
mkdir -p out/js
printf 'same-key artifact\n' >out/js/dyn_load_prime_driver.js
EOF
chmod +x "$scratch/bin/kio-a" "$scratch/bin/kio-b" "$scratch/bin/kio-same"

first_path=$(
  sh "$BUILD_DRIVER" "$scratch/bin/kio-a" "$src" "$assembly"
)
if [ "${first_path##*/}" != "dyn_load_prime_driver.js" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: publication changed the driver artifact basename\n' >&2
  exit 1
fi
if [ "$(cat "$first_path")" != "first artifact" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: first build returned the wrong artifact\n' >&2
  exit 1
fi

entered="$scratch/entered"
DYN_DRIVER_ENTERED="$entered" DYN_DRIVER_RELEASE="$release" \
  sh "$BUILD_DRIVER" "$scratch/bin/kio-b" "$src" "$assembly" \
    >"$scratch/second.path" 2>"$scratch/second.err" &
child=$!

wait_for_pause "$entered" "$child" "second distinct-key build" "$scratch/second.err"

if [ ! -r "$first_path" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: concurrent rebuild invalidated the first published path\n' >&2
  exit 1
fi

: >"$release"
wait "$child"
child=
second_path=$(cat "$scratch/second.path")
if [ "$first_path" = "$second_path" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: distinct compiler inputs shared one cache identity\n' >&2
  exit 1
fi
if [ "$(cat "$first_path")" != "first artifact" ] ||
   [ "$(cat "$second_path")" != "second artifact" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: a published artifact changed after a concurrent build\n' >&2
  exit 1
fi
reused_path=$(
  DYN_DRIVER_FAIL_IF_CALLED=1 \
    sh "$BUILD_DRIVER" "$scratch/bin/kio-a" "$src" "$assembly"
)
if [ "$reused_path" != "$first_path" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: identical inputs did not reuse the published artifact\n' >&2
  exit 1
fi

# Hold two identical-key builds after both have observed the same cold miss.
# They must publish one complete shared file and return the same stable path.
same_publication="$scratch/same-key-publication"
same_a_ready="$scratch/same-a-ready"
same_b_ready="$scratch/same-b-ready"
DYN_DRIVER_READY="$same_a_ready" DYN_DRIVER_RELEASE="$same_release" \
  sh "$BUILD_DRIVER" "$scratch/bin/kio-same" "$src" "$same_publication" \
    >"$scratch/same-a.path" 2>"$scratch/same-a.err" &
same_a=$!
wait_for_pause \
  "$same_a_ready" "$same_a" "first identical-key cold miss" "$scratch/same-a.err"

DYN_DRIVER_READY="$same_b_ready" DYN_DRIVER_RELEASE="$same_release" \
  sh "$BUILD_DRIVER" "$scratch/bin/kio-same" "$src" "$same_publication" \
    >"$scratch/same-b.path" 2>"$scratch/same-b.err" &
same_b=$!
wait_for_pause \
  "$same_b_ready" "$same_b" "second identical-key cold miss" "$scratch/same-b.err"

: >"$same_release"
wait_for_success "$same_a" "first identical-key build" "$scratch/same-a.err"
same_a=
wait_for_success "$same_b" "second identical-key build" "$scratch/same-b.err"
same_b=
same_a_path=$(cat "$scratch/same-a.path")
same_b_path=$(cat "$scratch/same-b.path")
if [ "$same_a_path" != "$same_b_path" ] ||
   [ "$(cat "$same_a_path")" != "same-key artifact" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: identical cold misses did not converge on one complete artifact\n' >&2
  exit 1
fi
set -- "$same_publication"/artifacts/*/dyn_load_prime_driver.js
if [ "$#" -ne 1 ] || [ ! -f "$1" ]; then
  printf 'dyn-load-prime-driver-publication-selftest: identical cold misses published more than one artifact\n' >&2
  exit 1
fi
if [ -d "$scratch/state/compiler/claims" ] &&
   find "$scratch/state/compiler/claims" -type f -print -quit | grep -q .; then
  printf 'dyn-load-prime-driver-publication-selftest: driver build left a compiler lease behind\n' >&2
  exit 1
fi

printf 'dyn-load-prime-driver-publication-selftest: ok\n'

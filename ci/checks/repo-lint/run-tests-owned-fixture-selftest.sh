#!/bin/sh

# Custom dependency-error fixtures must not make authored files cleanup targets,
# including when binaries or independent harnesses execute the same case.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
RUN_TESTS_SH=${RUN_TESTS_SH:-"$REPO_ROOT/ci/run-tests.sh"}
KIO_CI_PROGRESS_FD='' KIO_CI_TASK_NAME=''
export KIO_CI_PROGRESS_FD KIO_CI_TASK_NAME

scratch=$(mktemp -d)
pids=
cleanup() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  # Release every fixture before waiting, including after a failed assertion.
  for ready in "$scratch/events/"*.ready; do
    [ -f "$ready" ] || continue
    : >"${ready%.ready}.release"
  done
  for pid in $pids; do wait "$pid" 2>/dev/null || :; done
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fail() { printf 'run-tests-owned-fixture-selftest: %s\n' "$*" >&2; exit 1; }
mkdir -p "$scratch/bin" "$scratch/cases/marked/workdir/out" \
  "$scratch/cases/marked/library" "$scratch/tmp" "$scratch/cache" "$scratch/events"
fixture=$scratch/cases/marked

cat >"$scratch/bin/kio-a" <<'EOF'
#!/bin/sh
case "${1:-} ${2:-}" in
  'cache --help') exit 0 ;;
  'cache clear')
    printf '%s\n' "$PWD" >>"$FIXTURE_EVENTS/$FIXTURE_RUN.clear"
    : >cache-cleared
    exit 0 ;;
esac
exit 97
EOF
cp "$scratch/bin/kio-a" "$scratch/bin/kio-b"
chmod +x "$scratch/bin/kio-a" "$scratch/bin/kio-b"

cat >"$fixture/run.sh" <<'EOF'
#!/bin/sh
set -eu
cd workdir
[ "$(cat authored.kio)" = authored ]
[ "$(cat .hidden-input)" = hidden ]
[ "$(cat ../library/input)" = library ]
[ -L library-link ]
[ "$(cat library-link)" = library ]
[ "$(cat out/retained)" = ignored ]
[ ! -f generated ]
if [ "${FIXTURE_UPDATED:-0}" = 1 ]; then
  [ "$(cat checked)" = updated ]
fi
printf '%s\n' "$FIXTURE_RUN.$KIO_TARGET" >generated
event=$FIXTURE_EVENTS/$FIXTURE_RUN.$KIO_TARGET
printf '%s\n' "$PWD" >"$event.path"
if [ "${FIXTURE_WAIT:-0}" = 1 ]; then
  : >"$event.ready"
  tries=0
  until [ -f "$event.release" ]; do
    tries=$((tries + 1))
    [ "$tries" -le 30 ] || exit 98
    sleep 1
  done
  [ "$(cat generated)" = "$FIXTURE_RUN.$KIO_TARGET" ]
  [ "$(cat authored.kio)" = authored ]
fi
printf 'fixture output\n'
EOF
: >"$fixture/SKIP_DEP_MATERIALIZED"
printf 'fixture output\n' >"$fixture/expected.stdout"
printf '0\n' >"$fixture/expected.exit"
: >"$fixture/expected.stderr"
printf 'library\n' >"$fixture/library/input"
printf 'out/\n' >"$fixture/.gitignore"
git -C "$scratch/cases" init -q
git -C "$scratch/cases" add marked
# These inputs deliberately have no index entries, as with a case being authored.
printf 'authored\n' >"$fixture/workdir/authored.kio"
printf 'hidden\n' >"$fixture/workdir/.hidden-input"
printf 'ignored\n' >"$fixture/workdir/out/retained"
ln -s ../library/input "$fixture/workdir/library-link"

run_harness() {
  FIXTURE_EVENTS="$scratch/events" KIO_CI_SCHEDULE=DISABLE \
    TMPDIR="$scratch/tmp" sh "$RUN_TESTS_SH" \
      --cases-dir="$scratch/cases" --cache-base="$scratch/cache" \
      --impl-def="name=first,kio=$scratch/bin/kio-a,runner=true,target=js" \
      "$@" '^marked$'
}
assert_preserved() {
  [ -f "$fixture/workdir/authored.kio" ] || fail 'preexisting untracked authored input was deleted'
  [ "$(cat "$fixture/workdir/authored.kio")" = authored ] || fail 'authored input changed'
  [ "$(cat "$fixture/workdir/.hidden-input")" = hidden ] || fail 'hidden input changed'
  [ "$(cat "$fixture/workdir/out/retained")" = ignored ] || fail 'ignored input changed'
  [ ! -e "$fixture/workdir/generated" ] || fail 'generated output escaped execution scratch'
  [ ! -e "$fixture/workdir/cache-cleared" ] || fail 'cache clear ran on the original fixture'
}
assert_cleaned() {
  while IFS= read -r execution_path; do
    [ "$execution_path" != "$fixture/workdir" ] || fail 'custom execution used the original fixture'
    [ ! -e "$execution_path" ] || fail 'execution scratch survived the harness'
  done <"$1"
}
wait_ready() {
  attempts=0
  until [ -f "$1.ready" ]; do
    attempts=$((attempts + 1))
    [ "$attempts" -le 20 ] || fail "fixture did not reach barrier: $1"
    sleep 1
  done
}

if ! FIXTURE_RUN=single run_harness --jobs=1 >"$scratch/single.log" 2>&1; then
  cat "$scratch/single.log" >&2
  fail 'single fixture failed'
fi
assert_preserved
assert_cleaned "$scratch/events/single.js.path"
assert_cleaned "$scratch/events/single.clear"

# A fresh copy per implementation also prevents partial dependency material
# from leaking between sequential implementations that share one binary.
if ! FIXTURE_RUN=sequential run_harness --jobs=1 \
  --impl-def="name=second,kio=$scratch/bin/kio-a,runner=true,target=python" \
  >"$scratch/sequential.log" 2>&1; then
  cat "$scratch/sequential.log" >&2
  fail 'sequential implementations shared partial dependency material'
fi
[ "$(cat "$scratch/events/sequential.js.path")" != "$(cat "$scratch/events/sequential.python.path")" ] || \
  fail 'sequential implementations shared an execution tree'
assert_preserved
assert_cleaned "$scratch/events/sequential.js.path"
assert_cleaned "$scratch/events/sequential.python.path"

if ! FIXTURE_RUN=kept run_harness --keep-cache --jobs=1 >"$scratch/kept.log" 2>&1; then
  cat "$scratch/kept.log" >&2
  fail 'keep-cache fixture failed'
fi
[ ! -e "$scratch/events/kept.clear" ] || fail 'keep-cache cleared the copied cache'
assert_preserved

# Ownership is independent of Git tracking or checkout discovery.
mv "$scratch/cases/.git" "$scratch/fixture-index"

# Checks and golden updates still target the source case; execution sees their
# latest input bytes, including implementation-routed check updates.
cat >"$scratch/check.sh" <<'EOF'
#!/bin/sh
# ROUTING: impl
set -eu
[ "$PWD" = "$FIXTURE_ORIGINAL" ]
[ "$KIO_TEST_UPDATE" = 1 ]
printf 'updated\n' >workdir/checked
EOF
printf 'stale\n' >"$fixture/expected.stdout"
if ! FIXTURE_RUN=update FIXTURE_UPDATED=1 FIXTURE_ORIGINAL="$fixture" \
  run_harness --update-expected --check="$scratch/check.sh" >"$scratch/update.log" 2>&1; then
  cat "$scratch/update.log" >&2
  fail 'original-path checks or expected update failed'
fi
[ "$(cat "$fixture/expected.stdout")" = 'fixture output' ] || fail 'expected update missed the source case'
assert_preserved

FIXTURE_RUN=pair FIXTURE_WAIT=1 run_harness --jobs=2 \
  --impl-def="name=second,kio=$scratch/bin/kio-b,runner=true,target=python" \
  >"$scratch/pair.log" 2>&1 &
pair_pid=$! pids=$!
wait_ready "$scratch/events/pair.js"
wait_ready "$scratch/events/pair.python"
[ "$(cat "$scratch/events/pair.js.path")" != "$(cat "$scratch/events/pair.python.path")" ] || \
  fail 'different binaries shared an execution tree'
: >"$scratch/events/pair.js.release"
: >"$scratch/events/pair.python.release"
if ! wait "$pair_pid"; then cat "$scratch/pair.log" >&2; fail 'two-binary fixture failed'; fi
pids=
assert_preserved
assert_cleaned "$scratch/events/pair.js.path"
assert_cleaned "$scratch/events/pair.python.path"

FIXTURE_RUN=left FIXTURE_WAIT=1 run_harness --jobs=1 >"$scratch/left.log" 2>&1 &
left_pid=$! pids=$!
FIXTURE_RUN=right FIXTURE_WAIT=1 run_harness --jobs=1 >"$scratch/right.log" 2>&1 &
right_pid=$! pids="$pids $!"
wait_ready "$scratch/events/left.js"
wait_ready "$scratch/events/right.js"
left_path=$(cat "$scratch/events/left.js.path")
right_path=$(cat "$scratch/events/right.js.path")
[ "$left_path" != "$right_path" ] || fail 'independent harnesses shared an execution tree'
: >"$scratch/events/left.js.release"
if ! wait "$left_pid"; then cat "$scratch/left.log" >&2; fail 'first independent harness failed'; fi
pids=$right_pid
[ ! -e "$left_path" ] || fail 'finished harness retained its execution tree'
[ "$(cat "$right_path/generated")" = right.js ] || fail 'finished harness removed the live fixture'
: >"$scratch/events/right.js.release"
if ! wait "$right_pid"; then cat "$scratch/right.log" >&2; fail 'second independent harness failed'; fi
pids=
assert_preserved
assert_cleaned "$scratch/events/right.js.path"
printf 'run-tests-owned-fixture-selftest: ok\n'

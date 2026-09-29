#!/bin/sh
# shellcheck disable=SC2016
# Focused fixtures for the conventional emissions-corpus structure.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
LINT=$REPO_ROOT/ci/checks/repo-lint/emissions-corpus.sh
BACKENDS='js ts python java rust go swift haskell'

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/emissions-corpus-selftest.XXXXXX") || exit 2
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

make_case() {
  mc_root=$1
  mc_backend=$2
  mc_case=$mc_root/$mc_backend/case
  mkdir -p "$mc_case/workdir"
  printf 'build {\n  target %s {\n    out "out/%s/";\n  }\n}\n' \
    "$mc_backend" "$mc_backend" >"$mc_case/workdir/case.pkg.kio"
  printf 'module main;\n' >"$mc_case/workdir/main.kio"
  printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'scratch=$(mktemp -d "${TMPDIR:?}/emissions.fixture.XXXXXX")' \
    'trap '\''rm -rf "$scratch"'\'' EXIT INT TERM HUP' \
    'cp -R workdir "$scratch/workdir"' \
    'cd "$scratch/workdir"' \
    '"${KIO_BIN:?}" build "${KIO_TARGET:?}"' >"$mc_case/run.sh"
  : >"$mc_case/expected.stdout"
  : >"$mc_case/expected.stderr.ignore"
  printf '0\n' >"$mc_case/expected.exit"
  if [ "$mc_backend" = js ]; then
    : >"$mc_case/HOST_INTERFACE"
    mkdir "$mc_case/host"
    : >"$mc_case/host/host.js"
  else
    : >"$mc_case/ARTIFACT_SHAPE"
  fi
}

baseline=$scratch/baseline
mkdir "$baseline"
: >"$baseline/README.md"
for backend in $BACKENDS; do make_case "$baseline" "$backend"; done

fixture=$scratch/fixture
reset_fixture() {
  rm -rf "$fixture"
  cp -R "$baseline" "$fixture"
}

lint_log=$scratch/lint.log
run_lint() {
  KIO_EMISSIONS_CORPUS_ROOT=$fixture sh "$LINT" >"$lint_log" 2>&1
}

expect_pass() {
  if ! run_lint; then
    printf 'emissions-corpus-selftest: rejected %s\n' "$1" >&2
    sed -n '1,120p' "$lint_log" >&2
    exit 1
  fi
}

expect_fail() {
  ef_status=0
  run_lint || ef_status=$?
  if [ "$ef_status" -eq 0 ] || ! grep -Eq "$2" "$lint_log"; then
    printf 'emissions-corpus-selftest: failed expectation: %s\n' "$1" >&2
    sed -n '1,120p' "$lint_log" >&2
    exit 1
  fi
}

reset_fixture
expect_pass 'baseline corpus'

reset_fixture
mv "$fixture/js" "$fixture/lua"
expect_fail 'unknown backend' 'unknown corpus-root entry: lua'

reset_fixture
mv "$fixture/js/case" "$fixture/js/bad-name"
expect_fail 'nonconventional case name' 'not a conventional case name'

reset_fixture
rm "$fixture/js/case/HOST_INTERFACE"
expect_fail 'missing marker' 'exactly one empty HOST_INTERFACE or ARTIFACT_SHAPE'

reset_fixture
: >"$fixture/js/case/ARTIFACT_SHAPE"
expect_fail 'two markers' 'exactly one empty HOST_INTERFACE or ARTIFACT_SHAPE'

reset_fixture
printf 'x\n' >"$fixture/js/case/HOST_INTERFACE"
expect_fail 'nonempty marker' 'must be an empty regular file'

reset_fixture
printf '1\n' >"$fixture/js/case/expected.exit"
expect_fail 'nonzero exit' 'expected.exit must contain exactly 0'

reset_fixture
rm "$fixture/js/case/expected.exit"
expect_fail 'missing exit oracle' 'expected.exit must contain exactly 0'

reset_fixture
rm "$fixture/js/case/expected.stdout"
expect_fail 'missing stdout oracle' 'missing expected.stdout'

reset_fixture
: >"$fixture/js/case/expected.stderr"
expect_fail 'two stderr policies' 'exactly one expected.stderr policy'

reset_fixture
: >"$fixture/js/case/run.args"
expect_fail 'second execution mode' 'must use run.sh only'

reset_fixture
rm -rf "$fixture/js/case/host"
expect_fail 'HOST_INTERFACE without host' 'must contain host/'

reset_fixture
mkdir "$fixture/ts/case/host"
expect_fail 'ARTIFACT_SHAPE with host' 'must not contain host/'

reset_fixture
ln -s main.kio "$fixture/js/case/workdir/link.kio"
expect_fail 'symlink' 'must not contain symlinks'

reset_fixture
mkdir "$fixture/js/case/workdir/out"
expect_fail 'tracked output tree' 'must not contain an out/ tree'

reset_fixture
printf '%s\n' '"$KIO_RUNNER" out/js' >>"$fixture/js/case/run.sh"
expect_fail 'runner use' 'must not use a KIO runner'

reset_fixture
printf '%s\n' 'cd workdir' >>"$fixture/js/case/run.sh"
expect_fail 'tracked workdir cd' 'must not change into the tracked workdir'

reset_fixture
sed '/^scratch=/d' "$fixture/js/case/run.sh" >"$fixture/js/case/run.new"
mv "$fixture/js/case/run.new" "$fixture/js/case/run.sh"
expect_fail 'missing scratch' 'must create canonical TMPDIR scratch'

reset_fixture
sed '/^trap /d' "$fixture/js/case/run.sh" >"$fixture/js/case/run.new"
mv "$fixture/js/case/run.new" "$fixture/js/case/run.sh"
expect_fail 'missing cleanup' 'must directly trap cleanup'

reset_fixture
sed '/^cp -R workdir/d' "$fixture/js/case/run.sh" >"$fixture/js/case/run.new"
mv "$fixture/js/case/run.new" "$fixture/js/case/run.sh"
expect_fail 'missing copy' 'must directly copy workdir into scratch'

reset_fixture
sed '/KIO_BIN.*build/d' "$fixture/js/case/run.sh" >"$fixture/js/case/run.new"
mv "$fixture/js/case/run.new" "$fixture/js/case/run.sh"
expect_fail 'missing build' 'must directly build KIO_TARGET'

reset_fixture
cp "$fixture/js/case/workdir/case.pkg.kio" "$fixture/js/case/workdir/two.pkg.kio"
expect_fail 'two root manifests' 'exactly one root workdir/.*pkg.kio'

reset_fixture
sed 's/target js/target ts/' "$fixture/js/case/workdir/case.pkg.kio" \
  >"$fixture/js/case/workdir/case.new"
mv "$fixture/js/case/workdir/case.new" "$fixture/js/case/workdir/case.pkg.kio"
expect_fail 'root target mismatch' 'target must be js'

reset_fixture
printf '  target rust {\n  }\n' >>"$fixture/js/case/workdir/case.pkg.kio"
expect_fail 'multiple root targets' 'must declare exactly one build target'

reset_fixture
mkdir "$fixture/python/case/workdir/nested"
cp "$fixture/python/case/workdir/case.pkg.kio" \
  "$fixture/python/case/workdir/nested/nested.pkg.kio"
expect_fail 'nested ARTIFACT_SHAPE manifest' 'nested manifests are allowed only for HOST_INTERFACE'

reset_fixture
mkdir "$fixture/js/case/workdir/nested"
cp "$fixture/js/case/workdir/case.pkg.kio" \
  "$fixture/js/case/workdir/nested/nested.pkg.kio"
expect_pass 'same-target nested HOST_INTERFACE manifest'

sed 's/target js/target rust/' "$fixture/js/case/workdir/nested/nested.pkg.kio" \
  >"$fixture/js/case/workdir/nested/nested.new"
mv "$fixture/js/case/workdir/nested/nested.new" \
  "$fixture/js/case/workdir/nested/nested.pkg.kio"
expect_fail 'nested target mismatch' 'nested manifest .* target must be js'

printf 'emissions-corpus-selftest: ok\n'

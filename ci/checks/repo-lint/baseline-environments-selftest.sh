#!/bin/sh
# Exercise selection and execution plumbing with deterministic fake tools.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)
scratch_parent=${TMPDIR:-$repo_root/target}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/baseline-environments-selftest.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
fail() { printf 'baseline-environments-selftest: %s\n' "$*" >&2; exit 1; }
mkdir -p "$scratch/ci/infra" "$scratch/bin" "$scratch/tmp" "$scratch/cases/ordinary/workdir/out" \
  "$scratch/cases/fixed/workdir" "$scratch/cases/custom/workdir" "$scratch/cache"
cp "${RUN_TESTS_SH:-$repo_root/ci/run-tests.sh}" "$scratch/ci/run-tests.sh"
cp "$repo_root/ci/infra/"baseline-* "$repo_root/ci/infra/kio-compiler-proxy.sh" \
  "$repo_root/ci/infra/native-compiler-proxy.sh" "$repo_root/ci/infra/test-runner-identity-proxy.sh" \
  "$scratch/ci/infra/"
cp "$repo_root/ci/install-target-language-toolchains.sh" "$scratch/ci/"
cp "$repo_root/ci/infra/check-retained-output.sh" "$repo_root/ci/infra/typed-output-check.mjs" "$scratch/ci/infra/"
cp "$repo_root/ci/infra/check-baseline-compatibility.sh" "$scratch/ci/infra/"
mkdir -p "$scratch/ci/infra/backend-compatibility/retained"
cp -RP "$repo_root/ci/infra/backend-compatibility/retained/0.2.2" "$scratch/ci/infra/backend-compatibility/retained/"
cp "$repo_root/ci/infra/backend-compatibility/retained-node-host.mjs" \
  "$repo_root/ci/infra/backend-compatibility/node-module-context.json" "$scratch/ci/infra/backend-compatibility/"
cat >"$scratch/ci/infra/sccache.sh" <<'EOF'
kio_configure_sccache_environment() { :; }
kio_ensure_sccache_ready() { :; }
kio_any_sccache_wrapper() { return 1; }
EOF
cat >"$scratch/ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  --prepare) printf '%s/bin/runner\n' "$FIXTURE_ROOT"; exit 0 ;;
  --state-dir) printf '%s/schedule-state\n' "$FIXTURE_ROOT"; exit 0 ;;
  --available-parallelism) printf '1\n'; exit 0 ;;
  --readiness) exit 0 ;;
  --supervise)
    shift
    drained=
    while [ "$1" != -- ]; do
      case "$1" in --drained-marker) drained=$2 ;; esac
      shift 2
    done
    shift
    status=0; "$@" || status=$?
    : >"$drained"
    exit "$status" ;;
  --resource) shift 2 ;;
esac
[ "$1" != -- ] || shift
exec "$@"
EOF
cat >"$scratch/ci/impl-toolchain.sh" <<'EOF'
#!/bin/sh
set -eu
case "$1" in supported-languages) printf 'js\nts\n'; exit ;; esac
operation=$1 language=$2 environment=$4
printf '%s %s %s\n' "$operation" "$language" "$environment" >>"$FIXTURE_ROOT/provision.calls"
case "$operation" in
  toolchain-env)
    [ "${FIXTURE_MISSING:-}" != "$environment" ] || exit 2
    printf "export PATH='%s/tools/%s/bin:%s'\n" "$FIXTURE_ROOT" "$environment" "$PATH"
    printf "export KIO_TOOLCHAIN_ENVIRONMENT='%s' KIO_TOOLCHAIN_IDENTITY='fixture-%s'\n" "$environment" "$environment" ;;
  toolchain-plan|toolchain-install) printf 'fixture tool %s/%s\n' "$language" "$environment" ;;
  toolchain-exec)
    shift 4
    [ "$1" = -- ] || exit 2
    shift
    export KIO_TOOLCHAIN_ENVIRONMENT=$environment
    exec "$@" ;;
  *) exit 2 ;;
esac
EOF
cat >"$scratch/bin/kio" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}:${2:-}" in
  cache:*) exit 0 ;;
  dep:fetch)
    case "$PWD" in "$FIXTURE_ROOT/cases/"*) ;; *) exit 91 ;; esac
    printf '%s\n' "$PWD" >>"$FIXTURE_ROOT/fetch.calls"; exit 0 ;;
  debug:baseline-package)
    printf '%s %s %s\n' "$3" "$4" "$5" >>"$FIXTURE_ROOT/rewrite.calls"
    sed "s/baseline latest/baseline $5/" "$3"; exit 0 ;;
  test:*) exit 0 ;;
  build:*)
    printf '%s %s\n' "$PWD" "$KIO_TOOLCHAIN_ENVIRONMENT" >>"$FIXTURE_ROOT/build.calls"
    case "$PWD" in */ordinary/workdir) exit 92 ;; esac
    [ -f materialized.kio ]
    [ ! -f out/stale ]
    if [ -f baseline-links-fixture ]; then
      [ -f external.kio ] || { printf 'missing external source in baseline copy\n' >&2; exit 94; }
      [ ! -L external.kio ]
      [ "$(cat external.kio)" = 'shared source' ]
      printf 'private mutation\n' >external.kio
      newline_directory=$(printf 'prefix with spaces\n..')
      [ -f "$newline_directory/leaf.kio" ] && [ ! -L "$newline_directory/leaf.kio" ]
      [ "$(cat "$newline_directory/leaf.kio")" = 'shared source' ]
      [ -L directory.kio ] && [ -d directory.kio ] && [ ! -f directory.kio ]
      [ -L cycle-a.kio ] && [ ! -f cycle-a.kio ]
      [ -L cycle-b.kio ] && [ ! -f cycle-b.kio ]
      printf 'relocated collision\n' >../../../missing.kio
      [ -L dangling.kio ] && [ ! -f dangling.kio ]
    fi
    exit 0 ;;
esac
exit 93
EOF
cat >"$scratch/bin/runner" <<'EOF'
#!/bin/sh
printf 'ok\n'
EOF
cat >"$scratch/check.sh" <<'EOF'
#!/bin/sh
# ROUTING: impl
set -eu
case "$PWD" in
  */fixed) grep -q 'baseline latest' workdir/example.pkg.kio ;;
  */workdir-link) grep -q 'baseline latest' workdir/example.pkg.kio ;;
  */custom) exit 0 ;;
  *)
    grep -q 'baseline v5_0' workdir/example.pkg.kio
    [ ! -f workdir/out/stale ]
    tsc --version >>"$FIXTURE_ROOT/checker.calls" ;;
esac
EOF
for environment in development typescript5_0; do
  mkdir -p "$scratch/tools/$environment/bin"
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$environment" >"$scratch/tools/$environment/bin/tsc"
  chmod +x "$scratch/tools/$environment/bin/tsc"
done
chmod +x "$scratch/bin/kio" "$scratch/bin/runner"
for name in ordinary fixed custom; do
  printf 'package example;\nbuild {\n    target ts {\n        out "out/ts";\n        baseline latest;\n    }\n}\n' >"$scratch/cases/$name/workdir/example.pkg.kio"
  printf 'committed dependency\n' >"$scratch/cases/$name/workdir/materialized.kio"
  printf 'ok\n' >"$scratch/cases/$name/expected.stdout"
  printf '0\n' >"$scratch/cases/$name/expected.exit"
  : >"$scratch/cases/$name/expected.stderr"
done
: >"$scratch/cases/ordinary/run.args"
: >"$scratch/cases/fixed/run.args"
: >"$scratch/cases/fixed/FIXED_BASELINE"
printf '#!/bin/sh\nprintf "ok\\n"\n' >"$scratch/cases/custom/run.sh"
printf 'authored stale output\n' >"$scratch/cases/ordinary/workdir/out/stale"
printf 'shared source\n' >"$scratch/shared.kio"
mkdir "$scratch/shared-directory"
printf 'untraversed source\n' >"$scratch/shared-directory/hidden.kio"
: >"$scratch/cases/ordinary/workdir/baseline-links-fixture"
ln -s ../../../shared.kio "$scratch/cases/ordinary/workdir/external.kio"
ln -s ../../../shared-directory "$scratch/cases/ordinary/workdir/directory.kio"
ln -s ../../../missing.kio "$scratch/cases/ordinary/workdir/dangling.kio"
ln -s cycle-b.kio "$scratch/cases/ordinary/workdir/cycle-a.kio"
ln -s cycle-a.kio "$scratch/cases/ordinary/workdir/cycle-b.kio"
newline_directory=$(printf 'prefix with spaces\n..')
mkdir "$scratch/cases/ordinary/workdir/$newline_directory"
ln -s ../../../../shared.kio "$scratch/cases/ordinary/workdir/$newline_directory/leaf.kio"
export FIXTURE_ROOT="$scratch" KIO_CI_SCHEDULE=DISABLE KIO_CI_SCHEDULER_BIN="$scratch/bin/runner"
export TMPDIR="$scratch/tmp"
unset KIO_DEBUG_SAMPLE_IMPL_SEED KIO_DEBUG_SAMPLE_IMPL_SCOPE
run_fixture() {
  sh "$scratch/ci/run-tests.sh" --baseline-environments --impls=SAMPLE_IMPL --impl-seed=7 \
    --cases-dir="$scratch/cases" --cache-base="$scratch/cache" --jobs=1 \
    "--impl-def=name=kio@ts,kio=$scratch/bin/kio,runner=$scratch/bin/runner,target=ts" \
    "--check=$scratch/check.sh" "$@"
}
if ! run_fixture >"$scratch/first.log" 2>&1; then cat "$scratch/first.log" >&2; fail 'fixture execution failed'; fi
[ "$(cat "$scratch/shared.kio")" = 'shared source' ] || fail 'baseline copy wrote through source link'
[ -L "$scratch/cases/ordinary/workdir/external.kio" ] || fail 'source link was replaced'
[ ! -e "$scratch/missing.kio" ] || fail 'baseline copy created the source dangling target'
grep -q 'baseline latest' "$scratch/cases/ordinary/workdir/example.pkg.kio" || fail 'source selector mutated'
[ -f "$scratch/cases/ordinary/workdir/out/stale" ] || fail 'source output removed'
[ "$(wc -l <"$scratch/rewrite.calls" | tr -d ' ')" = 1 ] || fail 'fixed/custom selector subject was rewritten'
[ "$(wc -l <"$scratch/build.calls" | tr -d ' ')" = 2 ] || fail 'ordinary or fixed fixture did not build exactly once'
grep -q 'baseline=source-preserved' "$scratch/first.log" || fail 'preserved selector logged as resolved'
[ -s "$scratch/checker.calls" ] || fail 'selected checker did not execute'
selected_environment=$(awk -F '\t' '$2 == "ordinary" { print $6 }' "$scratch/first.log")
[ "$(sed -n '1p' "$scratch/checker.calls")" = "$selected_environment" ] || fail 'checker ignored selected endpoint'
if ! run_fixture >"$scratch/replay.log" 2>&1; then cat "$scratch/replay.log" >&2; fail 'replay failed'; fi
grep '^baseline-impl-map:' "$scratch/first.log" >"$scratch/first.map"
grep '^baseline-impl-map:' "$scratch/replay.log" >"$scratch/replay.map"
cmp "$scratch/first.map" "$scratch/replay.map" || fail 'seed did not replay choices'
for environment in development typescript5_0; do
  [ "$(grep -c "^toolchain-env ts $environment$" "$scratch/provision.calls" || :)" -le 2 ] || fail 'environment resolved per case'
done
if ! KIO_DEBUG_SAMPLE_IMPL_SCOPE=checks/orchestrators/contrib-tests \
    run_fixture --impl-seed=0 >"$scratch/contrib.log" 2>&1; then
  cat "$scratch/contrib.log" >&2; fail 'contrib fixture execution failed'
fi
awk -F '\t' '/^baseline-impl-map:/ && $6 != "development" { exit 1 }' "$scratch/contrib.log" || \
  fail 'untrusted contribution scope selected historical endpoint'
if KIO_DEBUG_SAMPLE_IMPL_SCOPE=checks/orchestrators/contrib-tests \
    run_fixture --toolchain-environment=typescript5_0 >"$scratch/contrib-override.log" 2>&1; then
  fail 'historical environment override bypassed contribution scope'
fi
grep -q 'contribution cases require their current-tested environments' "$scratch/contrib-override.log" || \
  fail 'historical contribution override was not diagnosed'
if FIXTURE_MISSING=development run_fixture >"$scratch/missing.log" 2>&1; then fail 'missing selected environment passed'; fi
grep -q 'selected baseline tool environment is unavailable' "$scratch/missing.log" || fail 'missing endpoint was not diagnosed'
if run_fixture --impl-seed=00 >"$scratch/invalid.log" 2>&1; then fail 'noncanonical seed passed'; fi
for backend in java haskell; do
  baseline=v21; [ "$backend" != haskell ] || baseline=ghc9_10
  environments=$(sh "$scratch/ci/infra/baseline-environments.sh" resolve "$backend" "$baseline" both)
  [ "$environments" = development ] || fail 'identical endpoint IDs were not deduplicated'
done
if sh "$scratch/ci/infra/baseline-environments.sh" resolve ts v99 minimum >/dev/null 2>&1; then fail 'unknown baseline passed'; fi
sh "$scratch/ci/install-target-language-toolchains.sh" ts --baseline v5_0 --endpoint both --plan >"$scratch/installer.log"
grep -q 'fixture tool ts/typescript5_0' "$scratch/installer.log" || fail 'installer omitted minimum'
grep -q 'fixture tool ts/development' "$scratch/installer.log" || fail 'installer omitted current endpoint'
sh "$scratch/ci/install-target-language-toolchains.sh" js ts --for-tests --plan >"$scratch/test-pool.log"
[ "$(grep -c '^fixture tool js/' "$scratch/test-pool.log")" = 1 ] || fail 'ordinary JS pool selected Node endpoints'
grep -q 'fixture tool ts/typescript5_0' "$scratch/test-pool.log" || fail 'test setup omitted selected checker'
grep -q 'fixture tool ts/development' "$scratch/test-pool.log" || fail 'test setup omitted current checker'
mkdir -p "$scratch/test-data/emissions/ts/rich_typed_host" "$scratch/ci/checks/orchestrators"
: >"$scratch/test-data/emissions/ts/rich_typed_host/HOST_INTERFACE"
cat >"$scratch/ci/checks/orchestrators/emissions-tests.sh" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" >>"$FIXTURE_ROOT/emissions.calls"
EOF
sh "$scratch/ci/infra/check-baseline-compatibility.sh" ts v5_0 --plan >"$scratch/compat-plan.log"
[ ! -e "$scratch/emissions.calls" ] || fail 'compatibility plan executed a case'
sh "$scratch/ci/infra/check-baseline-compatibility.sh" ts v5_0 >"$scratch/compat.log"
[ "$(grep -c '^--toolchain-environment=' "$scratch/emissions.calls")" = 2 ] || fail 'compatibility endpoints did not execute once each'
grep -Fxq -- '--toolchain-environment=typescript5_0' "$scratch/emissions.calls" || fail 'minimum checker endpoint not forwarded'
grep -Fxq -- '--toolchain-environment=development' "$scratch/emissions.calls" || fail 'current checker endpoint not forwarded'
[ "$(grep -Fxc '^ts/rich_typed_host$' "$scratch/emissions.calls")" = 2 ] || fail 'compatibility witness was not exact'
cat >"$scratch/bin/retained-runner" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = --package-name ] && [ "$2" = ffi_export_poly_roundtrip ]
[ "$3" = --protocol ] && [ "$4" = export-poly-roundtrip ]
case "$5" in "$FIXTURE_ROOT/ci/infra/"*) exit 97 ;; esac
[ -f "$5/ffi_export_poly_roundtrip.js" ]
printf 'consumed\n' >>"$FIXTURE_ROOT/retained.calls"
printf 'poly-string\n42\nleft\n'
EOF
chmod +x "$scratch/bin/retained-runner"
sh "$scratch/ci/infra/check-retained-output.sh" js es2020 "$scratch/bin/retained-runner" >"$scratch/retained.log"
[ "$(wc -l <"$scratch/retained.calls" | tr -d ' ')" = 1 ] || fail 'retained output was not consumed once'
printf '\n' >>"$scratch/ci/infra/backend-compatibility/retained/0.2.2/artifacts/js/ffi_export_poly_roundtrip.js"
if sh "$scratch/ci/infra/check-retained-output.sh" js es2020 "$scratch/bin/retained-runner" >"$scratch/integrity.log" 2>&1; then
  fail 'retained artifact mutation passed integrity check'
fi
[ "$(wc -l <"$scratch/retained.calls" | tr -d ' ')" = 1 ] || fail 'corrupt retained output reached runner'
cp "$repo_root/ci/infra/backend-compatibility/retained/0.2.2/artifacts/js/ffi_export_poly_roundtrip.js" \
  "$scratch/ci/infra/backend-compatibility/retained/0.2.2/artifacts/js/ffi_export_poly_roundtrip.js"
printf 'unexpected module\n' >"$scratch/ci/infra/backend-compatibility/retained/0.2.2/artifacts/js/unrecorded.js"
if sh "$scratch/ci/infra/check-retained-output.sh" js es2020 "$scratch/bin/retained-runner" >"$scratch/inventory.log" 2>&1; then
  fail 'unrecorded retained artifact passed integrity check'
fi
grep -q 'frozen file inventory differs' "$scratch/inventory.log" || fail 'unrecorded artifact was not diagnosed'

# Two baselines produce two FULL rows, not four endpoint rows. SAMPLE keeps
# one row and gives both baselines/endpoints a deterministic reachable draw.
printf 'ts v5_1 typescript5_1 development checker-and-locked-quickjs\n' >>"$scratch/ci/infra/baseline-environments.tsv"
printf '1\t1\t/cases/example\tkio@ts\t/compiler\t/runner\tts\t\t\texample\n' >"$scratch/rows.tsv"
printf '/cases/example\tparametric\n' >"$scratch/policies.tsv"
sh "$scratch/ci/infra/baseline-environments.sh" expand "$scratch/rows.tsv" "$scratch/policies.tsv" \
  full 0 fixture /cases "$scratch/full.tsv"
[ "$(wc -l <"$scratch/full.tsv" | tr -d ' ')" = 2 ] || fail 'FULL multiplied endpoint count'
awk -F '\t' '$12 != "development" { exit 1 }' "$scratch/full.tsv" || fail 'FULL did not use current endpoint'
: >"$scratch/draws.tsv"
seed=0
while [ "$seed" -lt 64 ]; do
  sh "$scratch/ci/infra/baseline-environments.sh" expand "$scratch/rows.tsv" "$scratch/policies.tsv" \
    sample "$seed" fixture /cases "$scratch/draw.tsv"
  [ "$(wc -l <"$scratch/draw.tsv" | tr -d ' ')" = 1 ] || fail 'SAMPLE multiplied execution count'
  cut -f 11,12 "$scratch/draw.tsv" >>"$scratch/draws.tsv"
  seed=$((seed + 1))
done
for selection in 'v5_0 typescript5_0' 'v5_0 development' 'v5_1 typescript5_1' 'v5_1 development'; do
  selection_baseline=${selection%% *}
  selection_environment=${selection#* }
  awk -F '\t' -v baseline="$selection_baseline" -v environment="$selection_environment" '$1 == baseline && $2 == environment { found = 1 } END { exit !found }' \
    "$scratch/draws.tsv" || fail "unreachable alternative $selection"
done
printf '/cases/example\tcurrent-only\n' >"$scratch/policies.tsv"
sh "$scratch/ci/infra/baseline-environments.sh" expand "$scratch/rows.tsv" "$scratch/policies.tsv" \
  sample 0 fixture /cases "$scratch/current.tsv"
awk -F '\t' '$12 != "development" { exit 1 }' "$scratch/current.tsv" || fail 'contribution policy selected historical tool'
mkdir -p "$scratch/cases/workdir-link" "$scratch/root-source"
cp "$scratch/cases/ordinary/workdir/example.pkg.kio" "$scratch/root-source/example.pkg.kio"
cp "$scratch/cases/ordinary/workdir/materialized.kio" "$scratch/root-source/materialized.kio"
ln -s ../../root-source "$scratch/cases/workdir-link/workdir"
: >"$scratch/cases/workdir-link/run.test-only"
: >"$scratch/cases/workdir-link/expected.stdout"
: >"$scratch/cases/workdir-link/expected.stderr"
printf '0\n' >"$scratch/cases/workdir-link/expected.exit"
before_rewrites=$(wc -l <"$scratch/rewrite.calls" | tr -d ' ')
if ! run_fixture '^workdir-link$' >"$scratch/workdir-link.log" 2>&1; then
  cat "$scratch/workdir-link.log" >&2; fail 'directory-symlink workdir fixture failed'
fi
[ "$(wc -l <"$scratch/rewrite.calls" | tr -d ' ')" = "$before_rewrites" ] || \
  fail 'directory-symlink workdir entered baseline rewriting'
grep -q 'baseline=source-preserved' "$scratch/workdir-link.log" || \
  fail 'directory-symlink workdir lost its source-preserved policy'
grep -q 'baseline latest' "$scratch/root-source/example.pkg.kio" || \
  fail 'directory-symlink workdir source selector changed'
printf 'baseline-environments-selftest: ok\n'

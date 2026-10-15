#!/bin/sh
# Frozen published output is consumed opaquely by the ordinary fixed runner.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)

if [ "${1:-}" = --__consume ]; then
  language=$2 runner=$3 artifact=$4 scratch=$5
  case "$language" in
    js|ts)
      cp "$script_dir/backend-compatibility/node-module-context.json" "$scratch/package.json"
      node "$script_dir/backend-compatibility/retained-node-host.mjs" "$artifact" >"$scratch/node.stdout"
      cmp "$script_dir/backend-compatibility/retained/0.2.2/expected.stdout" "$scratch/node.stdout"
      printf 'retained-node: public host passed on %s\n' "$(node --version)" >&2
      ;;
  esac
  case "$language" in
    ts|python)
      node -e 'const fs = require("node:fs"); const [root, backend] = process.argv.slice(1); fs.writeFileSync(root + "/plan.json", JSON.stringify([{package_root:root,target:backend,backend,output_dir:root+"/artifact",namespace:"ffi_export_poly_roundtrip"}]));' \
        "$scratch" "$language"
      checker=tsc
      [ "$language" != python ] || checker=pyright
      set -- "$checker"
      if [ "$language" = ts ]; then set -- "$@" --strict --noEmit; fi
      node "$script_dir/typed-output-check.mjs" check "$scratch" "$scratch/plan.json" "$language" "$@"
      ;;
  esac
  KIO_TEST_RUNNER_CACHE_DISABLE=1 "$runner" --package-name ffi_export_poly_roundtrip \
    --protocol export-poly-roundtrip "$artifact"
  exit
fi

[ "$#" -ge 3 ] || {
  printf 'Usage: sh ci/infra/check-retained-output.sh LANGUAGE BASELINE RUNNER [--plan]\n' >&2
  exit 2
}
language=$1 baseline=$2 runner=$3
shift 3
plan=0
case "$#:${1:-}" in 0:) ;; 1:--plan) plan=1 ;; *) printf 'error: unknown retained-output arguments\n' >&2; exit 2 ;; esac
environment=$(sh "$script_dir/baseline-environments.sh" resolve "$language" "$baseline" current-tested)
fixture=$script_dir/backend-compatibility/retained/0.2.2
[ -d "$fixture/artifacts/$language" ] || { printf 'error: no retained output for %s\n' "$language" >&2; exit 2; }
printf 'retained-output: producer=0.2.2 backend=%s baseline=%s environment=%s\n' "$language" "$baseline" "$environment"
case "$language" in
  js) printf 'retained-output: runtimes=Cargo-locked-QuickJS,selected-Node\n' ;;
  ts) printf 'retained-output: runtimes=Cargo-locked-QuickJS,selected-Node checker=selected-tsc\n' ;;
esac
sh "$repo_root/ci/impl-toolchain.sh" toolchain-plan "$language" --environment "$environment"
[ "$plan" -eq 0 ] || exit 0
[ -x "$runner" ] || { printf 'error: supply an existing immutable runner executable\n' >&2; exit 2; }
case "$runner" in /*) ;; *) runner=$(CDPATH='' cd -- "$(dirname -- "$runner")" && pwd)/${runner##*/} ;; esac
verify_fixture() (
  cd "$fixture"
  retained_paths=$(find artifacts source run.args expected.stdout expected.exit expected.stderr.ignore -type f -print | LC_ALL=C sort)
  manifest_paths=$(awk '{ print $2 }' SHA256SUMS | LC_ALL=C sort)
  if [ "$retained_paths" != "$manifest_paths" ] || [ -n "$(find artifacts source -type l -print)" ]; then
    printf 'retained-output: frozen file inventory differs from its manifest\n' >&2
    exit 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -c SHA256SUMS >/dev/null
  else shasum -a 256 -c SHA256SUMS >/dev/null; fi
)
verify_fixture
scratch_parent=${TMPDIR:-$repo_root/target}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/retained-output.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
cp -RP "$fixture/artifacts/$language" "$scratch/artifact"
if ! sh "$repo_root/ci/impl-toolchain.sh" toolchain-exec "$language" --environment "$environment" -- \
    sh "$script_dir/check-retained-output.sh" --__consume "$language" "$runner" "$scratch/artifact" "$scratch" \
    >"$scratch/stdout" 2>"$scratch/stderr"; then
  cat "$scratch/stdout" "$scratch/stderr" >&2
  printf 'retained-output: native/checker consumption failed\n' >&2
  exit 1
fi
if ! cmp "$fixture/expected.stdout" "$scratch/stdout"; then
  cat "$scratch/stdout" >&2
  printf 'retained-output: protocol output differs\n' >&2
  exit 1
fi
sed -n '/^toolchain:/p; /^toolchain-verified:/p; /^retained-node:/p' "$scratch/stderr" >&2
verify_fixture
printf 'retained-output: pass (published artifact hashes preserved; no generation)\n'

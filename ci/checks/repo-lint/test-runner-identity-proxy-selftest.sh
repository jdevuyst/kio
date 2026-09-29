#!/bin/sh

# Causal contract test for the harness-private runner identity proxy.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
PROXY=${PROXY:-"$REPO_ROOT/ci/infra/test-runner-identity-proxy.sh"}

scratch=$(mktemp -d)
cleanup() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

cat >"$scratch/delegate" <<'EOF'
#!/bin/sh
i=0
for arg do
  i=$((i + 1))
  printf '%d:%s\n' "$i" "$arg"
done
EOF
chmod +x "$scratch/delegate"

run_proxy() {
  rp_name=$1
  rp_default=$2
  shift 2
  cp "$PROXY" "$scratch/$rp_name"
  chmod +x "$scratch/$rp_name"
  printf '%s\n%s\n' "$scratch/delegate" "$rp_default" >"$scratch/$rp_name.config"
  "$scratch/$rp_name" "$@"
}

run_proxy default outer --protocol 'value with spaces' 'meta;$*?[]' >"$scratch/actual"
cat >"$scratch/expected" <<'EOF'
1:--package-name
2:outer
3:--protocol
4:value with spaces
5:meta;$*?[]
EOF
cmp "$scratch/expected" "$scratch/actual"

run_proxy split outer --package-name nested --artifact-namespace ns out >"$scratch/actual"
cat >"$scratch/expected" <<'EOF'
1:--package-name
2:nested
3:--artifact-namespace
4:ns
5:out
EOF
cmp "$scratch/expected" "$scratch/actual"

run_proxy equals outer --package-name=nested out >"$scratch/actual"
printf '%s\n' '1:--package-name=nested' '2:out' >"$scratch/expected"
cmp "$scratch/expected" "$scratch/actual"

run_proxy ordered outer \
  --package-name first --artifact-namespace first-ns first-out \
  --package-name=second --artifact-namespace second-ns second-out \
  >"$scratch/actual"
cat >"$scratch/expected" <<'EOF'
1:--package-name
2:first
3:--artifact-namespace
4:first-ns
5:first-out
6:--package-name=second
7:--artifact-namespace
8:second-ns
9:second-out
EOF
cmp "$scratch/expected" "$scratch/actual"

run_proxy no-default '' --protocol p out >"$scratch/actual"
printf '%s\n' '1:--protocol' '2:p' '3:out' >"$scratch/expected"
cmp "$scratch/expected" "$scratch/actual"

for retired_name in \
  "KIO_TEST_RUNNER_""REAL" \
  "KIO_TEST_PACKAGE_""NAME"; do
  if git -C "$REPO_ROOT" grep -n "$retired_name" -- ci test-data TESTING.md; then
    printf 'test-runner-identity-proxy-selftest: retired runner handle/config leaked into the corpus boundary: %s\n' \
      "$retired_name" >&2
    exit 1
  fi
done

printf 'test-runner-identity-proxy-selftest: ok\n'

#!/bin/sh
# Hermetic regression test for dependency-materialization write ordering.
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
repo="$scratch/repo"
mkdir -p "$repo/ci/checks/repo-lint"
mkdir -p "$repo/ci/infra"
cp "$REPO_ROOT/ci/checks/repo-lint/dep-materialization.sh" \
  "$repo/ci/checks/repo-lint/dep-materialization.sh"
cp "$REPO_ROOT/ci/infra/dep-materialization-order.sh" \
  "$repo/ci/infra/dep-materialization-order.sh"

make_node() {
  node=$1
  local_name=$2
  source=$3
  terminator=$4
  mkdir -p "$repo/test-data/$node/workdir/$local_name"
  cat > "$repo/test-data/$node/workdir/$local_name.dep.kio" <<EOF
dependency $local_name;

source {
  path "$source"$terminator
}
EOF
  printf 'package %s;\n' "$local_name" \
    > "$repo/test-data/$node/workdir/$local_name.pkg.kio"
  printf 'stale\n' > "$repo/test-data/$node/workdir/$local_name/state"
}

# Lexical order is the reverse of dependency order. The middle edge uses the
# current canonical path form, whose final separator is optional.
make_node a_top top ../../m_middle/workdir/middle.pkg.kio ';'
make_node m_middle middle ../../z_leaf/workdir/leaf.pkg.kio ''
make_node z_leaf leaf ../../library/leaf.pkg.kio ';'
mkdir -p "$repo/test-data/library"
printf 'package leaf;\n' > "$repo/test-data/library/leaf.pkg.kio"

cat > "$scratch/kio" <<'EOF'
#!/bin/sh
node=$(basename "$(dirname "$PWD")")
printf '%s\n' "$node" >> "$ORDER_LOG"
[ "${FAIL_NODE:-}" != "$node" ] || exit 23
case "$node" in
  z_leaf) printf 'fresh\n' > leaf/state ;;
  m_middle) cp ../../z_leaf/workdir/leaf/state middle/state ;;
  a_top) cp ../../m_middle/workdir/middle/state top/state ;;
  *) exit 24 ;;
esac
EOF
chmod +x "$scratch/kio"

(
  cd "$repo"
  git init --quiet -b main .
  git config user.email test@kio.invalid
  git config user.name 'Kio Test'
  git add -A
  git commit --quiet -m fixture
)

: > "$scratch/order"
ORDER_LOG="$scratch/order" KIO_BIN="$scratch/kio" \
  sh "$repo/ci/checks/repo-lint/dep-materialization.sh" --write >/dev/null
expected='z_leaf
m_middle
a_top'
actual=$(cat "$scratch/order")
if [ "$actual" != "$expected" ]; then
  printf 'dep-materialization-order-selftest: wrong write order\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi
if [ "$(cat "$repo/test-data/a_top/workdir/top/state")" != fresh ]; then
  printf 'dep-materialization-order-selftest: top closure remained stale\n' >&2
  exit 1
fi

rc=0
FAIL_NODE=m_middle ORDER_LOG="$scratch/order" KIO_BIN="$scratch/kio" \
  sh "$repo/ci/checks/repo-lint/dep-materialization.sh" --write >/dev/null 2>&1 || rc=$?
if [ "$rc" -ne 23 ]; then
  printf 'dep-materialization-order-selftest: fetch status 23 became %s\n' "$rc" >&2
  exit 1
fi

# An in-scope cycle fails before any tree is rewritten.
cat > "$repo/test-data/z_leaf/workdir/leaf.dep.kio" <<'EOF'
dependency leaf;

source {
  path "../../a_top/workdir/top.pkg.kio";
}
EOF
: > "$scratch/order"
rc=0
ORDER_LOG="$scratch/order" KIO_BIN="$scratch/kio" \
  sh "$repo/ci/checks/repo-lint/dep-materialization.sh" --write \
  >"$scratch/cycle.out" 2>"$scratch/cycle.err" || rc=$?
if [ "$rc" -eq 0 ] || [ -s "$scratch/order" ]; then
  printf 'dep-materialization-order-selftest: cycle did not fail before writes\n' >&2
  exit 1
fi
if ! grep -F 'dependency cycle' "$scratch/cycle.err" >/dev/null; then
  printf 'dep-materialization-order-selftest: cycle diagnostic was missing\n' >&2
  exit 1
fi

# A local source in its own dependency-bearing directory is a self-cycle,
# not a node with no ordering edge.
cat > "$repo/test-data/z_leaf/workdir/leaf.dep.kio" <<'EOF'
dependency leaf;

source {
  path "leaf.pkg.kio";
}
EOF
: > "$scratch/order"
rc=0
ORDER_LOG="$scratch/order" KIO_BIN="$scratch/kio" \
  sh "$repo/ci/checks/repo-lint/dep-materialization.sh" --write \
  >"$scratch/self-cycle.out" 2>"$scratch/self-cycle.err" || rc=$?
if [ "$rc" -eq 0 ] || [ -s "$scratch/order" ]; then
  printf 'dep-materialization-order-selftest: self-cycle did not fail before writes\n' >&2
  exit 1
fi
if ! grep -F 'dependency cycle' "$scratch/self-cycle.err" >/dev/null; then
  printf 'dep-materialization-order-selftest: self-cycle diagnostic was missing\n' >&2
  exit 1
fi

# A deliberately non-materializable fixture is outside the write graph and
# cannot block unrelated regeneration with its descriptor shape.
touch "$repo/test-data/z_leaf/SKIP_DEP_MATERIALIZED"
: > "$scratch/order"
ORDER_LOG="$scratch/order" KIO_BIN="$scratch/kio" \
  sh "$repo/ci/checks/repo-lint/dep-materialization.sh" --write >/dev/null
expected='m_middle
a_top'
actual=$(cat "$scratch/order")
if [ "$actual" != "$expected" ]; then
  printf 'dep-materialization-order-selftest: skipped node affected write graph\nexpected:\n%s\nactual:\n%s\n' \
    "$expected" "$actual" >&2
  exit 1
fi

printf 'dep-materialization-order-selftest: ok\n'

#!/bin/sh
# Both unsealed source projection and sealed history replay resolve bodyless
# contracts. Their compile-time type imports do not authorize Prime execution.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

scratch=$(mktemp -d) || fail "cannot make scratch directory"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

KIO_CACHE_HOME="$scratch/kio-cache"
export KIO_CACHE_HOME
unset XDG_CACHE_HOME

GIT_AUTHOR_NAME='Kio Test'
GIT_AUTHOR_EMAIL='test@kio.invalid'
GIT_COMMITTER_NAME='Kio Test'
GIT_COMMITTER_EMAIL='test@kio.invalid'
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

for context in unsealed sealed; do
  src="$scratch/library-$context"
  consumer="$scratch/consumer-$context"
  bare="$scratch/$context.git"
  cp -R "library/$context" "$src" || fail "cannot copy $context dependency"
  cp -R workdir "$consumer" || fail "cannot copy $context consumer"
  (
    cd "$src" || exit 1
    git init --quiet -b main . || exit 1
    git add -A || exit 1
    GIT_AUTHOR_DATE='2020-01-01T00:00:00 +0000' \
    GIT_COMMITTER_DATE='2020-01-01T00:00:00 +0000' \
      git commit --quiet -m 'contract fixture' || exit 1
  ) || fail "cannot assemble $context dependency"
  git clone --quiet --bare "$src" "$bare" || fail "cannot create $context repository"

  cat > "$consumer/contract.dep.kio" <<EOF
dependency contract;

source {
  git "$bare";
  ref "main";
}
EOF

  (
    cd "$consumer" || exit 1
    "$KIO_BIN" dep fetch >fetch.out 2>fetch.err
  ) || fail "$context contract fetch failed: $(cat "$consumer/fetch.err")"
  grep -q 'sig "' "$consumer/contract.lock.kio" \
    || fail "$context contract produced no digest"
  printf '%s contract ok\n' "$context"
done

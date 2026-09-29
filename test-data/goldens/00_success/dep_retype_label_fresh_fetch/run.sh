#!/bin/sh
# Fresh fetches cover counterpart providers on both sides of dependency order.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

case_temp=$(mktemp -d) || fail "cannot create case directory"
trap 'rm -rf "$case_temp"' EXIT HUP INT TERM
cp -R fixtures/library "$case_temp/library" || fail "cannot copy libraries"
cp -R fixtures/earlier "$case_temp/earlier" || fail "cannot copy earlier provider consumer"
cp -R fixtures/later "$case_temp/later" || fail "cannot copy later provider consumer"

for order in earlier later; do
  (
    cd "$case_temp/$order" || fail "cannot enter consumer"
    case "$order" in
      earlier) provider=acore ;;
      later) provider=zcore ;;
      *) fail "unknown provider order" ;;
    esac
    mv widget.dep.kio.in widget.dep.kio \
      || fail "cannot instantiate source dependency declaration"
    mv "$provider.dep.kio.in" "$provider.dep.kio" \
      || fail "cannot instantiate counterpart dependency declaration"
    test ! -e widget || fail "source dependency was materialized before fresh fetch"
    test ! -e "$provider" || fail "counterpart provider was materialized before fresh fetch"
    "$KIO_BIN" dep fetch >fetch.out 2>fetch.err \
      || fail "fresh fetch failed: $(cat fetch.err)"
    "$KIO_BIN" dep fetch >repeat.out 2>repeat.err \
      || fail "repeated fetch failed: $(cat repeat.err)"
    "$KIO_BIN" dep fetch --force >forced.out 2>forced.err \
      || fail "forced fetch failed: $(cat forced.err)"
    "$KIO_BIN" dep fetch >final.out 2>final.err \
      || fail "final fetch failed: $(cat final.err)"
    "$KIO_BIN" check >check.out 2>check.err \
      || fail "fresh materialized source failed checking: $(cat check.err)"
    "$KIO_BIN" test >test.out 2>test.err \
      || fail "materialized identity laws failed: $(cat test.err)"
  ) || exit 1
  printf '%s provider: fresh label identity\n' "$order"
done

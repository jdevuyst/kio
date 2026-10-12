#!/bin/sh
# Check source-crate preparation, manual-source guards, and Cargo forwarding.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/manual packaging.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir -p "$scratch/ci" "$scratch/kio-rs/src" "$scratch/docs" "$scratch/specs"
cp "$REPO_ROOT/ci/package-kio.sh" "$scratch/ci/"
printf 'guide\n' >"$scratch/KIO-FOR-AGENTS.md"
printf 'page\n' >"$scratch/docs/page.md"
printf 'grammar\n' >"$scratch/specs/grammar.md"
printf 'manifest\n' >"$scratch/kio-rs/Cargo.toml"
printf 'source\n' >"$scratch/kio-rs/src/lib.rs"
printf '/src/ignored.rs\n/manual/\n' >"$scratch/kio-rs/.gitignore"
git -C "$scratch" init -q
git -C "$scratch" add -- docs specs KIO-FOR-AGENTS.md kio-rs
git -C "$scratch" -c user.name=Fixture -c user.email=fixture@example.invalid commit -qm fixture
cat >"$scratch/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
if [ "${FIXTURE_REQUIRE_OFFLINE:-}" = yes ]; then
  [ "${CARGO_NET_OFFLINE:-}" = true ]
fi
printf '%s\n' "$*" >>"$FIXTURE_CARGO_LOG"
case "$1" in
  run) [ "$*" = 'run --locked --example pack_manual -- --write' ] ;;
  package)
    if [ "$*" = 'package --list --allow-dirty --locked' ]; then
      cat "$FIXTURE_CARGO_INVENTORY"
    else
      exit "${FIXTURE_CARGO_EXIT:-0}"
    fi
    ;;
  publish) exit "${FIXTURE_CARGO_EXIT:-0}" ;;
  *) exit 90 ;;
esac
EOF
FIXTURE_CARGO_LOG=$scratch/cargo.log
FIXTURE_CARGO_INVENTORY=$scratch/inventory
export FIXTURE_CARGO_LOG FIXTURE_CARGO_INVENTORY
printf '%s\n' Cargo.toml Cargo.toml.orig .cargo_vcs_info.json src/lib.rs manual/index.tsv manual/payload.tar.gz-concat >"$FIXTURE_CARGO_INVENTORY"
sh "$scratch/ci/package-kio.sh" package --locked --no-verify --offline
[ "$(sed -n '1p' "$FIXTURE_CARGO_LOG")" = 'run --locked --example pack_manual -- --write' ]
[ "$(sed -n '2p' "$FIXTURE_CARGO_LOG")" = 'package --list --allow-dirty --locked' ]
[ "$(sed -n '3p' "$FIXTURE_CARGO_LOG")" = 'package --locked --no-verify --offline --allow-dirty' ]
status=0
FIXTURE_CARGO_EXIT=37 sh "$scratch/ci/package-kio.sh" publish --dry-run --locked || status=$?
[ "$status" -eq 37 ]
[ "$(sed -n '6p' "$FIXTURE_CARGO_LOG")" = 'publish --dry-run --locked --allow-dirty' ]
tr '/' '\134' <"$FIXTURE_CARGO_INVENTORY" >"$scratch/inventory-windows"
FIXTURE_CARGO_INVENTORY=$scratch/inventory-windows FIXTURE_REQUIRE_OFFLINE=yes \
  OS=Windows_NT MSYSTEM='' sh "$scratch/ci/package-kio.sh" package --frozen --no-verify
[ "$(sed -n '9p' "$FIXTURE_CARGO_LOG")" = 'package --frozen --no-verify --allow-dirty' ]
status=0
FIXTURE_CARGO_INVENTORY=$scratch/inventory-windows OS='' MSYSTEM='' \
  sh "$scratch/ci/package-kio.sh" package --locked >"$scratch/error" 2>&1 || status=$?
[ "$status" -eq 1 ]
case "$(cat "$scratch/error")" in *'invalid package source path'*) ;; *) exit 1 ;; esac

reject_package_source() {
  : >"$FIXTURE_CARGO_LOG"
  if sh "$scratch/ci/package-kio.sh" package --locked --allow-dirty >"$scratch/error" 2>&1; then
    printf '%s\n' 'dirty or untracked package sources were packaged' >&2
    exit 1
  fi
  [ "$(wc -l <"$FIXTURE_CARGO_LOG" | tr -d ' ')" = "$1" ]
  case "$(cat "$scratch/error")" in *'package sources must be tracked'*|*'package sources must be regular files'*|*'must not be symlinks'*) ;; *) exit 1 ;; esac
}
printf 'changed\n' >>"$scratch/kio-rs/src/lib.rs"
reject_package_source 0
printf 'source\n' >"$scratch/kio-rs/src/lib.rs"
git -C "$scratch" mv kio-rs/src/lib.rs kio-rs/src/renamed.rs
reject_package_source 0
git -C "$scratch" mv kio-rs/src/renamed.rs kio-rs/src/lib.rs
mv "$scratch/kio-rs/src/lib.rs" "$scratch/source-original"
reject_package_source 0
mv "$scratch/source-original" "$scratch/kio-rs/src/lib.rs"
for extra in src/new.rs src/ignored.rs 'src/[l]ib.rs'; do
  printf 'new\n' >"$scratch/kio-rs/$extra"
  printf '%s\n' "$extra" >>"$FIXTURE_CARGO_INVENTORY"
  reject_package_source 2
  sed '$d' "$FIXTURE_CARGO_INVENTORY" >"$scratch/inventory-next"
  mv "$scratch/inventory-next" "$FIXTURE_CARGO_INVENTORY"
done
ln -s lib.rs "$scratch/kio-rs/src/link.rs"
git -C "$scratch" add -- kio-rs/src/link.rs
git -C "$scratch" -c user.name=Fixture -c user.email=fixture@example.invalid commit -qm symlink
printf '%s\n' src/link.rs >>"$FIXTURE_CARGO_INVENTORY"
reject_package_source 2
sed '$d' "$FIXTURE_CARGO_INVENTORY" >"$scratch/inventory-next"
mv "$scratch/inventory-next" "$FIXTURE_CARGO_INVENTORY"
for selection in '--manifest-path=elsewhere/Cargo.toml' '--workspace' '--package=elsewhere'; do
  : >"$FIXTURE_CARGO_LOG"
  status=0
  sh "$scratch/ci/package-kio.sh" package "$selection" >"$scratch/error" 2>&1 || status=$?
  [ "$status" -eq 2 ] && [ ! -s "$FIXTURE_CARGO_LOG" ]
done

reject_before_cargo() {
  : >"$FIXTURE_CARGO_LOG"
  if sh "$scratch/ci/package-kio.sh" package --locked >"$scratch/error" 2>&1; then
    printf '%s\n' 'dirty manual sources were packaged' >&2
    exit 1
  fi
  [ ! -s "$FIXTURE_CARGO_LOG" ]
  case "$(cat "$scratch/error")" in *'manual sources have uncommitted changes'*) ;; *) exit 1 ;; esac
}
printf 'changed\n' >>"$scratch/docs/page.md"
reject_before_cargo
printf 'page\n' >"$scratch/docs/page.md"
git -C "$scratch" mv docs/page.md docs/renamed.md
reject_before_cargo
git -C "$scratch" mv docs/renamed.md docs/page.md
printf 'changed\n' >>"$scratch/KIO-FOR-AGENTS.md"
git -C "$scratch" add -- KIO-FOR-AGENTS.md
reject_before_cargo
printf '%s\n' 'manual-package-selftest: preparation, flags, exit status, and tracked input guards pass'

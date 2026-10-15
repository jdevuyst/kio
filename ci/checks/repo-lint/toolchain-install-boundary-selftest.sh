#!/bin/sh
# Exercise the shared install handoff, not a replacement Swift downloader.
set -eu
case "$(uname -s)" in
  Linux) ;;
  *) printf 'toolchain install boundary: Linux GPG handoff test skipped on this OS\n'; exit 0 ;;
esac
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
if [ -z "${KIO_TEST_PROVISION_SOURCE:-}" ]; then
  sed 's/GNUPGHOME="[^\"]*" MISE_SWIFT_GPG_VERIFY=1 //' "$repo_root/tools/toolchains/provision.sh" >"$work/unfixed.sh"
  sh -n "$work/unfixed.sh"
  if KIO_TEST_PROVISION_SOURCE="$work/unfixed.sh" sh "$0" >"$work/unfixed.log" 2>&1; then
    printf 'error: fix-disabled Swift install did not expose its caller keyring\n' >&2; exit 1
  fi
  grep -q 'native Swift verification handoff:' "$work/unfixed.log"
  printf 'fix-disabled Swift handoff: caller GPG root rejected as expected\n'
fi
mkdir "$work/caller-gpg" "$work/store"
printf '%s\n' untouched >"$work/caller-gpg/keyring-witness"
cat >"$work/native-mise" <<'EOF'
#!/bin/sh
set -eu
if [ "$KIO_TEST_NATIVE_PLATFORM" = linux ]; then
  printf 'native Swift verification handoff: %s\n' "$GNUPGHOME" >&2
  [ "$GNUPGHOME" = "$KIO_TEST_NATIVE_ROOT/swift/gnupg" ]
  [ "$MISE_SWIFT_GPG_VERIFY" = 1 ]
  : >"$GNUPGHOME/native-import-witness"
else
  [ "$GNUPGHOME" = "$KIO_TEST_CALLER_GPG" ]
  [ "$MISE_SWIFT_GPG_VERIFY" = 0 ]
fi
[ "$*" = 'install --locked -y swift@6.4.0' ]
EOF
chmod +x "$work/native-mise"
# shellcheck source=tools/toolchains/provision.sh disable=SC1091
. "${KIO_TEST_PROVISION_SOURCE:-$repo_root/tools/toolchains/provision.sh}"
emit_extra_system_packages() { :; }
toolchain_version() { printf '%s\n' 6.4.0; }
# The sourced toolchain_install_selected consumes these selection globals.
# shellcheck disable=SC2034
TC_TOOLS=swift TC_LANGUAGES=swift TC_PLATFORM=linux-x64 \
  TC_ROOT=$work/store TC_MISE=$work/native-mise
export GNUPGHOME="$work/caller-gpg" MISE_SWIFT_GPG_VERIFY=0
export KIO_TEST_NATIVE_ROOT="$TC_ROOT" KIO_TEST_CALLER_GPG="$GNUPGHOME" KIO_TEST_NATIVE_PLATFORM=linux
toolchain_install_selected
[ "$GNUPGHOME" = "$work/caller-gpg" ]
[ "$MISE_SWIFT_GPG_VERIFY" = 0 ]
[ "$(cat "$work/caller-gpg/keyring-witness")" = untouched ]
[ -f "$TC_ROOT/swift/gnupg/native-import-witness" ]
[ "$(find "$TC_ROOT/swift/gnupg" -prune -perm 0700 -print)" = "$TC_ROOT/swift/gnupg" ]
# shellcheck disable=SC2034
TC_PLATFORM=macos-arm64 KIO_TEST_NATIVE_PLATFORM=macos
toolchain_install_selected
printf 'toolchain install boundary: Linux Swift owns GPG state/enforces native verification; caller and macOS inputs preserved\n'

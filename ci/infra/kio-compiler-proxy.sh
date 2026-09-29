#!/bin/sh
#
# Admit semantic Kio compiler commands launched by the corpus harness.
#
# POSIX sh only.

set -eu

proxy_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
tool=${0##*/}
real_tool=
if [ -f "$proxy_dir/real/$tool.path" ]; then
  IFS= read -r real_tool <"$proxy_dir/real/$tool.path" || real_tool=
fi

if [ -z "$real_tool" ] || [ ! -x "$real_tool" ]; then
  printf 'error: Kio compiler proxy has no resolved %s command\n' "$tool" >&2
  exit 2
fi

# Kio's global --no-cache flag is accepted anywhere. Classify the first two
# remaining words conservatively: semantic and unknown commands take a permit;
# cheap formatting, repository-management, and CLI-information commands do
# not retain one for their whole process. The exact package-roundtrip helper
# only parses and renders a copied manifest: terminal inapplicable cases need no
# permit, while applicable cases enter an admitted build immediately afterward
# and need no extra admission turn first. Other debug commands remain admitted.
subcommand=
debug_subcommand=
for arg in "$@"; do
  [ "$arg" = --no-cache ] && continue
  if [ -z "$subcommand" ]; then
    subcommand=$arg
  else
    debug_subcommand=$arg
    break
  fi
done

admit=1
case "$subcommand" in
  ''|-h|--help|-V|--version|fmt|cache|dep|init|completions)
    admit=0
    ;;
  debug)
    [ "$debug_subcommand" = kio-prime-roundtrip-package ] && admit=0
    ;;
esac

if [ "$admit" = 0 ]; then
  exec "$real_tool" "$@"
fi

IFS= read -r admission <"$proxy_dir/admission-path" || {
  printf 'error: Kio compiler proxy has no admission command\n' >&2
  exit 2
}
[ -n "$admission" ] || {
  printf 'error: Kio compiler proxy has an empty admission command\n' >&2
  exit 2
}
# The semantic Kio compiler typechecks and emits host source; it does not invoke
# the test runner's configured native-compiler wrapper. Tell the generic facade
# that this current command cannot start that daemon. schedule.sh consumes the
# marker before launching the compiler, so a nested command must classify
# itself independently.
KIO_CI_SCHEDULE_READINESS=SKIP
export KIO_CI_SCHEDULE_READINESS
exec sh "$admission" --resource compiler -- "$real_tool" "$@"

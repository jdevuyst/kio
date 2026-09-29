#!/bin/sh
# Forward a corpus runner invocation to the harness-configured delegate,
# injecting the top-level package identity only when the caller did not supply
# one explicitly. `ci/run-tests.sh` places two lines beside this private copy:
# the delegate path and the optional default package name.
set -u

config=$0.config
if [ ! -f "$config" ]; then
  printf 'test-runner-identity-proxy: missing harness configuration: %s\n' "$config" >&2
  exit 2
fi
delegate=$(sed -n '1p' "$config")
default_package=$(sed -n '2p' "$config")
if [ -z "$delegate" ]; then
  printf 'test-runner-identity-proxy: empty runner delegate\n' >&2
  exit 2
fi

has_package=0
for arg do
  case "$arg" in
    --package-name|--package-name=*) has_package=1 ;;
  esac
done

if [ "$has_package" = 0 ] && [ -n "$default_package" ]; then
  exec "$delegate" --package-name "$default_package" "$@"
fi
exec "$delegate" "$@"

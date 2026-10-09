#!/bin/sh
# Static Bash completion leaves build name slots empty, including under errexit.
set -eu
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
"$KIO_BIN" completions bash >"$scratch/completion.bash"
touch "$scratch/path_not_a_target"
cd "$scratch"
bash -eu -c '
  . ./completion.bash
  for option in --package --pkg; do
    COMP_WORDS=(kio build web "$option" "")
    COMP_CWORD=4
    _kio
    test -z "${COMPREPLY[*]-}"
  done
  COMP_WORDS=(kio build path)
  COMP_CWORD=2
  _kio
  test -z "${COMPREPLY[*]-}"
  COMP_WORDS=(kio build -- path)
  COMP_CWORD=3
  _kio
  test -z "${COMPREPLY[*]-}"
  COMP_WORDS=(kio build --pkg app web --p)
  COMP_CWORD=5
  _kio
  printf "%s\n" "${COMPREPLY[@]}"
'

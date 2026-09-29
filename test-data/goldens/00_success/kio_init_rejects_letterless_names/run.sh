#!/bin/sh
# Invalid explicit and directory-derived names must fail before scaffolding.
set -eu

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
failed=0
for mode in explicit inferred; do
  for name in _ _1 _1_2; do
    package_dir=$scratch/$mode/$name
    mkdir -p "$package_dir"
    status=0
    if [ "$mode" = explicit ]; then
      (cd "$package_dir" && "$KIO_BIN" init "$name") \
        >"$scratch/stdout" 2>"$scratch/stderr" || status=$?
    else
      (cd "$package_dir" && "$KIO_BIN" init) \
        >"$scratch/stdout" 2>"$scratch/stderr" || status=$?
    fi
    if [ "$status" -ne 2 ] || [ ! -s "$scratch/stderr" ] ||
       [ -n "$(find "$package_dir" ! -path "$package_dir" -print)" ]; then
      printf 'invalid %s name %s: expected usage error and no created files\n' \
        "$mode" "$name" >&2
      failed=1
    fi
  done
  for name in _a _a1; do
    package_dir=$scratch/$mode/$name
    mkdir -p "$package_dir"
    if [ "$mode" = explicit ]; then
      (cd "$package_dir" && "$KIO_BIN" init "$name") >/dev/null
    else
      (cd "$package_dir" && "$KIO_BIN" init) >/dev/null
    fi
    [ -f "$package_dir/$name.pkg.kio" ]
    [ -f "$package_dir/main.kio" ]
    (cd "$package_dir" && "$KIO_BIN" check)
  done
done
[ "$failed" -eq 0 ]
printf 'init rejects letterless names before writing; marked names check clean\n'

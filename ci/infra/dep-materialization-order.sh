#!/bin/sh
# Shared dependency-before-dependent ordering for materialization directories.
# Sourced by the bulk writer and the per-case canonicality check.

dep_materialization_order() {
  dmo_remaining=$1
  dmo_ordered=
  dmo_root=$(pwd -P)
  while [ -n "$dmo_remaining" ]; do
    dmo_next=
    dmo_progress=0
    for dmo_dir in $dmo_remaining; do
      dmo_blocked=0
      for dmo_file in "$dmo_dir"/*.dep.kio; do
        dmo_source=$(sed -n 's/^[[:space:]]*path[[:space:]]*"\([^"]*\)"[[:space:]]*;\{0,1\}[[:space:]]*$/\1/p' "$dmo_file")
        [ -n "$dmo_source" ] || continue
        case "$dmo_source" in
          /*) dmo_path=$dmo_source ;;
          *) dmo_path=$(dirname "$dmo_file")/$dmo_source ;;
        esac
        [ -f "$dmo_path" ] || continue
        dmo_source_dir=$(CDPATH='' cd -- "$(dirname "$dmo_path")" && pwd -P)
        for dmo_producer in $dmo_remaining; do
          if [ "$dmo_root/$dmo_producer" = "$dmo_source_dir" ]; then
            dmo_blocked=1
            break
          fi
        done
        [ "$dmo_blocked" = 0 ] || break
      done
      if [ "$dmo_blocked" = 1 ]; then
        dmo_next="$dmo_next${dmo_next:+
}$dmo_dir"
      else
        dmo_ordered="$dmo_ordered${dmo_ordered:+
}$dmo_dir"
        dmo_progress=1
      fi
    done
    if [ "$dmo_progress" = 0 ]; then
      printf 'dependency cycle among materialized test packages:\n' >&2
      printf '%s\n' "$dmo_remaining" | sed 's/^/  /' >&2
      return 1
    fi
    dmo_remaining=$dmo_next
  done
  printf '%s\n' "$dmo_ordered"
}

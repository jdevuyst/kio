#!/bin/sh
#
# Admit native compiler commands launched by custom corpus scripts.
#
# POSIX sh only.

set -eu

proxy_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
tool=${0##*/}
real_tool=$proxy_dir/real/$tool

[ -x "$real_tool" ] || {
  printf 'error: native compiler proxy has no resolved %s command\n' "$tool" >&2
  exit 2
}

admit=1
if [ "$tool" = rustc ] && [ "$#" = 2 ] &&
   [ "$1" = --version ] && [ "$2" = --verbose ]; then
  admit=0
elif [ "$tool" = go ]; then
  subcommand=
  tool_command=
  tool_option_value=0
  tool_options_known=1
  global_options_known=1
  list_compiles=0
  skip_next=0
  for arg in "$@"; do
    if [ "$skip_next" = 1 ]; then
      skip_next=0
      continue
    fi
    if [ -z "$subcommand" ]; then
      case "$arg" in
        -C) skip_next=1 ;;
        -C=*) ;;
        -*) global_options_known=0 ;;
        *) subcommand=$arg ;;
      esac
      continue
    fi
    if [ "$subcommand" = tool ] && [ -z "$tool_command" ]; then
      if [ "$tool_option_value" = 1 ]; then
        tool_option_value=0
        continue
      fi
      case "$arg" in
        -C|-modfile|-overlay)
          tool_option_value=1
          ;;
        -C=*|-modfile=*|-overlay=*|-modcacherw|-n)
          ;;
        -*)
          tool_options_known=0
          ;;
        *)
          tool_command=$arg
          ;;
      esac
      continue
    fi
    case "$subcommand:$arg" in
      list:-export|list:-export=*|list:-compiled|list:-compiled=*)
        list_compiles=1
        ;;
    esac
  done
  if [ "$global_options_known" = 0 ]; then
    admit=1
  else
    case "$subcommand" in
    build|install|run|test|vet) ;;
    list) [ "$list_compiles" = 1 ] || admit=0 ;;
    tool)
      if [ "$tool_option_value" = 0 ] && [ "$tool_options_known" = 1 ]; then
        case "$tool_command" in
          asm|cgo|compile|dist|link) ;;
          cover|fix) admit=0 ;;
          *) admit=1 ;;
        esac
      fi
      ;;
    ''|bug|clean|doc|env|fix|fmt|generate|get|help|mod|telemetry|version|work)
      admit=0
      ;;
    *) admit=1 ;;
    esac
  fi
fi

if [ "$admit" = 0 ]; then
  exec "$real_tool" "$@"
fi

IFS= read -r admission <"$proxy_dir/admission-path" || {
  printf 'error: native compiler proxy has no admission command\n' >&2
  exit 2
}
[ -n "$admission" ] || {
  printf 'error: native compiler proxy has an empty admission command\n' >&2
  exit 2
}
exec sh "$admission" --resource compiler -- "$real_tool" "$@"

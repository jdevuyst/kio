#!/bin/sh
# This case deliberately combines full-Surface annotation clients (including
# destructuring and monadic `do`) in one public diagnostic matrix. It therefore
# carries no IS_KIO_PRIME marker; Prime planner parity is covered by the
# focused internal policy tests.
set -u

cd workdir || exit
mkdir -p out

# shellcheck disable=SC2016 # backticks are literal diagnostic text
primary='type placeholder `_` cannot appear beneath a `forall` inside an annotation'
secondary='this annotation introduces the enclosing binder'
# shellcheck disable=SC2016 # backticks are literal diagnostic text
help='write a concrete type beneath this binder, or make the whole annotation slot `_`'
provider_location='provider.kio:3:26'
provider_source='pub type Wrapped[Slot] = [Provider_bound] Slot -> Provider_bound;'
failed=0

for probe in \
  direct_lambda_parameter \
  direct_lambda_return \
  direct_local_unary \
  direct_local_destructure \
  direct_local_as_pattern \
  direct_monadic_do_unary \
  direct_monadic_do_pattern \
  alias_root_crossing_parameter \
  alias_transitive_parameter \
  alias_imported_provider \
  alias_imported_provider_growth \
  alias_duplicate_mixed \
  alias_reorder_local
do
  stdout="out/$probe.stdout"
  stderr="out/$probe.stderr"
  if (cd "$probe" && "$KIO_BIN" --no-cache check) >"$stdout" 2>"$stderr"; then
    printf '%s unexpectedly succeeded\n' "$probe" >&2
    failed=1
    continue
  else
    status=$?
  fi

  if [ "$status" -ne 14 ]; then
    printf '%s exited %s instead of 14\n' "$probe" "$status" >&2
    cat "$stderr" >&2
    failed=1
    continue
  fi
  if [ -s "$stdout" ]; then
    printf '%s wrote unexpected stdout\n' "$probe" >&2
    cat "$stdout" >&2
    failed=1
  fi
  for expected in "$primary" "$secondary" "$help"; do
    count=$(grep -F -c "$expected" "$stderr" || true)
    if [ "$count" -ne 1 ]; then
      printf '%s emitted %s copies of required diagnostic line: %s\n' \
        "$probe" "$count" "$expected" >&2
      failed=1
    fi
  done
  case "$probe" in
    alias_imported_provider|alias_imported_provider_growth)
      provider_count=$(grep -F -c 'provider.kio' "$stderr" || true)
      if [ "$provider_count" -ne 1 ]; then
        printf '%s emitted %s provider-file references instead of 1\n' \
          "$probe" "$provider_count" >&2
        failed=1
      fi
      if grep -F -q 'other.kio' "$stderr"; then
        printf '%s misattributed the binder to other.kio\n' "$probe" >&2
        failed=1
      fi
      for provider_fact in "$provider_location" "$provider_source"; do
        fact_count=$(grep -F -c "$provider_fact" "$stderr" || true)
        if [ "$fact_count" -ne 1 ]; then
          printf '%s emitted %s copies of provider fact: %s\n' \
            "$probe" "$fact_count" "$provider_fact" >&2
          failed=1
        fi
      done
      if grep -F -q 'source unavailable' "$stderr"; then
        printf '%s failed to render the available provider source\n' \
          "$probe" >&2
        failed=1
      fi
      ;;
  esac
  cat "$stderr" >&2
done

if ! cmp -s \
  out/alias_imported_provider.stderr \
  out/alias_imported_provider_growth.stderr
then
  printf '%s\n' \
    'adding a disconnected same-spelled alias changed the imported-provider diagnostic' >&2
  failed=1
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi
exit 14

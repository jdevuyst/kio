# shellcheck shell=sh
# shellcheck disable=SC2016
# Conventional structural checks shared by the emissions orchestrator and lint.

EMISSIONS_BACKENDS='js ts python java rust go swift haskell'

emissions_contract_fail() {
  printf '%s: %s\n' "${EMISSIONS_CONTRACT_PREFIX:-emissions-corpus}" "$1" >&2
  emissions_contract_status=1
}

emissions_backend_is_known() {
  emissions_known_wanted=$1
  for emissions_known_backend in $EMISSIONS_BACKENDS; do
    [ "$emissions_known_backend" = "$emissions_known_wanted" ] && return 0
  done
  return 1
}

emissions_validate_manifest() {
  emissions_manifest=$1
  emissions_backend=$2
  emissions_label=$3
  emissions_targets=$(awk '$1 == "target" && $3 == "{" { print $2 }' "$emissions_manifest")
  emissions_target_count=$(printf '%s\n' "$emissions_targets" | awk 'NF { n++ } END { print n + 0 }')
  if [ "$emissions_target_count" -ne 1 ]; then
    emissions_contract_fail "$emissions_label must declare exactly one build target"
  elif [ "$emissions_targets" != "$emissions_backend" ]; then
    emissions_contract_fail "$emissions_label target must be $emissions_backend (found $emissions_targets)"
  fi
}

emissions_validate_run_script() {
  emissions_script=$1
  emissions_label=$2

  if grep -Eq 'KIO_RUNNER' "$emissions_script"; then
    emissions_contract_fail "$emissions_label must not use a KIO runner"
  fi
  if grep -Eq "^[[:space:]]*cd[[:space:]]+['\"]?(\\./)?workdir([/'\"[:space:]]|$)" "$emissions_script"; then
    emissions_contract_fail "$emissions_label must not change into the tracked workdir"
  fi

  if ! grep -Eq '^scratch=\$\(mktemp -d "\$\{TMPDIR:\?\}/[A-Za-z0-9._-]*XXXXXX"\)$' "$emissions_script"; then
    emissions_contract_fail "$emissions_label must create canonical TMPDIR scratch in scratch"
  fi
  if ! grep -Eq '^trap '\''rm -rf "\$scratch"'\'' (EXIT INT TERM HUP|0 HUP INT TERM)$' "$emissions_script"; then
    emissions_contract_fail "$emissions_label must directly trap cleanup of scratch"
  fi

  if grep -Eq '^cp -R workdir "\$scratch/workdir"$' "$emissions_script"; then
    if ! grep -Eq '^[[:space:]]*cd "\$scratch/workdir"$' "$emissions_script"; then
      emissions_contract_fail "$emissions_label must directly enter the copied workdir"
    fi
  elif grep -Eq '^cp -R workdir/\. "\$scratch/"$' "$emissions_script"; then
    if ! grep -Eq '^[[:space:]]*cd "\$scratch"$' "$emissions_script"; then
      emissions_contract_fail "$emissions_label must directly enter the copied workdir"
    fi
  else
    emissions_contract_fail "$emissions_label must directly copy workdir into scratch"
  fi

  if ! grep -Eq '^[[:space:]]*"\$\{KIO_BIN:\?\}" build "\$\{KIO_TARGET:\?\}"$' "$emissions_script"; then
    emissions_contract_fail "$emissions_label must directly build KIO_TARGET with KIO_BIN"
  fi
}

emissions_validate_case() {
  emissions_case=$1
  emissions_backend=$2
  emissions_root=$3
  emissions_rel=${emissions_case#"$emissions_root"/}

  emissions_bad_link=$(find "$emissions_case" -type l -print 2>/dev/null | sed -n '1p')
  if [ -n "$emissions_bad_link" ]; then
    emissions_contract_fail "$emissions_rel must not contain symlinks"
  fi
  emissions_out=$(find "$emissions_case" -type d -name out -print 2>/dev/null | sed -n '1p')
  if [ -n "$emissions_out" ]; then
    emissions_contract_fail "$emissions_rel must not contain an out/ tree"
  fi

  emissions_marker_count=0
  emissions_marker=
  for emissions_candidate in HOST_INTERFACE ARTIFACT_SHAPE; do
    if [ -e "$emissions_case/$emissions_candidate" ] || [ -L "$emissions_case/$emissions_candidate" ]; then
      emissions_marker_count=$((emissions_marker_count + 1))
      emissions_marker=$emissions_candidate
      if [ ! -f "$emissions_case/$emissions_candidate" ] \
        || [ -L "$emissions_case/$emissions_candidate" ] \
        || [ -s "$emissions_case/$emissions_candidate" ]; then
        emissions_contract_fail "$emissions_rel/$emissions_candidate must be an empty regular file"
      fi
    fi
  done
  if [ "$emissions_marker_count" -ne 1 ]; then
    emissions_contract_fail "$emissions_rel must carry exactly one empty HOST_INTERFACE or ARTIFACT_SHAPE marker"
  fi

  if [ "$emissions_marker" = HOST_INTERFACE ]; then
    if [ ! -d "$emissions_case/host" ] || [ -L "$emissions_case/host" ]; then
      emissions_contract_fail "$emissions_rel is HOST_INTERFACE and must contain host/"
    fi
  elif [ "$emissions_marker" = ARTIFACT_SHAPE ] \
    && { [ -e "$emissions_case/host" ] || [ -L "$emissions_case/host" ]; }; then
    emissions_contract_fail "$emissions_rel is ARTIFACT_SHAPE and must not contain host/"
  fi

  if [ ! -f "$emissions_case/run.sh" ] || [ -L "$emissions_case/run.sh" ] \
    || [ -e "$emissions_case/run.args" ] || [ -e "$emissions_case/run.test-only" ]; then
    emissions_contract_fail "$emissions_rel must use run.sh only"
  else
    emissions_validate_run_script "$emissions_case/run.sh" "$emissions_rel/run.sh"
  fi
  if [ -e "$emissions_case/KNOWN_FAILING" ]; then
    emissions_contract_fail "$emissions_rel is success-only and must not carry KNOWN_FAILING"
  fi
  if [ ! -f "$emissions_case/expected.stdout" ]; then
    emissions_contract_fail "$emissions_rel is missing expected.stdout"
  fi
  emissions_exit=
  if [ -f "$emissions_case/expected.exit" ]; then
    emissions_exit=$(tr -d '[:space:]' <"$emissions_case/expected.exit")
  fi
  if [ "$emissions_exit" != 0 ]; then
    emissions_contract_fail "$emissions_rel/expected.exit must contain exactly 0"
  fi
  emissions_stderr_count=0
  for emissions_stderr in expected.stderr expected.stderr.ignore expected.stderr.grep; do
    [ -f "$emissions_case/$emissions_stderr" ] \
      && emissions_stderr_count=$((emissions_stderr_count + 1))
  done
  if [ "$emissions_stderr_count" -ne 1 ]; then
    emissions_contract_fail "$emissions_rel must carry exactly one expected.stderr policy file"
  fi

  emissions_root_count=0
  emissions_root_manifest=
  for emissions_manifest in "$emissions_case"/workdir/*.pkg.kio; do
    [ -f "$emissions_manifest" ] || continue
    emissions_root_count=$((emissions_root_count + 1))
    emissions_root_manifest=$emissions_manifest
  done
  if [ "$emissions_root_count" -ne 1 ]; then
    emissions_contract_fail "$emissions_rel must contain exactly one root workdir/*.pkg.kio"
  else
    emissions_validate_manifest "$emissions_root_manifest" "$emissions_backend" "$emissions_rel root manifest"
  fi

  emissions_manifests=$(find "$emissions_case/workdir" -type f -name '*.pkg.kio' -print 2>/dev/null)
  while IFS= read -r emissions_manifest; do
    [ -n "$emissions_manifest" ] || continue
    emissions_manifest_rel=${emissions_manifest#"$emissions_case/workdir"/}
    case "$emissions_manifest_rel" in
      */*)
        if [ "$emissions_marker" != HOST_INTERFACE ]; then
          emissions_contract_fail "$emissions_rel nested manifests are allowed only for HOST_INTERFACE"
        fi
        emissions_validate_manifest "$emissions_manifest" "$emissions_backend" \
          "$emissions_rel nested manifest $emissions_manifest_rel"
        ;;
    esac
  done <<EOF
$emissions_manifests
EOF
}

emissions_validate_corpus() {
  emissions_root=$1
  emissions_contract_status=0
  if [ ! -d "$emissions_root" ] || [ -L "$emissions_root" ]; then
    emissions_contract_fail "missing emissions corpus directory: $emissions_root"
    return 1
  fi

  for emissions_entry in "$emissions_root"/* "$emissions_root"/.[!.]* "$emissions_root"/..?*; do
    [ -e "$emissions_entry" ] || [ -L "$emissions_entry" ] || continue
    emissions_name=${emissions_entry##*/}
    [ "$emissions_name" = README.md ] && continue
    if ! emissions_backend_is_known "$emissions_name"; then
      emissions_contract_fail "unknown corpus-root entry: $emissions_name"
    elif [ ! -d "$emissions_entry" ] || [ -L "$emissions_entry" ]; then
      emissions_contract_fail "$emissions_name must be a real backend directory"
    fi
  done

  for emissions_backend in $EMISSIONS_BACKENDS; do
    emissions_backend_dir=$emissions_root/$emissions_backend
    if [ ! -d "$emissions_backend_dir" ] || [ -L "$emissions_backend_dir" ]; then
      emissions_contract_fail "missing backend bucket: $emissions_backend"
      continue
    fi
    emissions_case_count=0
    for emissions_case in "$emissions_backend_dir"/* "$emissions_backend_dir"/.[!.]* "$emissions_backend_dir"/..?*; do
      [ -e "$emissions_case" ] || [ -L "$emissions_case" ] || continue
      emissions_case_name=${emissions_case##*/}
      case "$emissions_case_name" in
        ''|*[!a-z0-9_]*)
          emissions_contract_fail "$emissions_backend/$emissions_case_name is not a conventional case name"
          continue
          ;;
      esac
      if [ ! -d "$emissions_case" ] || [ -L "$emissions_case" ]; then
        emissions_contract_fail "$emissions_backend/$emissions_case_name must be a real case directory"
        continue
      fi
      emissions_case_count=$((emissions_case_count + 1))
      emissions_validate_case "$emissions_case" "$emissions_backend" "$emissions_root"
    done
    if [ "$emissions_case_count" -eq 0 ]; then
      emissions_contract_fail "$emissions_backend must contain at least one emissions case"
    fi
  done
  return "$emissions_contract_status"
}

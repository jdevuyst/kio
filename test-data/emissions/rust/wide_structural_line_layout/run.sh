#!/bin/sh
# SUBJECT: Rust's measured wide-sum conversion keeps every generated physical source line within 8,000 characters.
# CONTRACT: Recorded migration measurement: 9,423 characters before 6d9f51748ff8c3407ca42e8ad83366bbbd1efbf4 and 2,848 after e738792e18be647bed876d19b41590d6a3d3f35a; the latter narrows the repair to outbound conversion.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust-wide-line.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

file=out/rust/src/lib.rs
if [ ! -f "$file" ]; then
  printf 'wide_structural_line_layout: missing generated Rust library\n' >&2
  exit 1
fi
sum_case=$(grep -F '___mod_13_testapi_smain_sum_ucase()' "$file" || true)
if ! printf '%s' "$sum_case" | grep -qF 'crate::shapes::KioSum<'; then
  printf 'wide_structural_line_layout: wide outbound sum marker did not fire\n' >&2
  exit 1
fi
if ! printf '%s' "$sum_case" | grep -qF 'as crate::shapes::KioType>::from_stored'; then
  printf 'wide_structural_line_layout: wide outbound sum conversion did not fire\n' >&2
  exit 1
fi
max_line=$(awk '{ length_now = length($0); if (length_now > maximum) maximum = length_now } END { print maximum + 0 }' "$file")
if [ "$max_line" -gt 8000 ]; then
  printf 'wide_structural_line_layout: generated Rust line is %s characters (limit 8000)\n' "$max_line" >&2
  exit 1
fi

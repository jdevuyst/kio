#!/bin/sh
# SUBJECT: A mutually recursive public newtype boundary keeps exact nominal facades while its erased storage stays private.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
lib_rs=$(cat out/rust/src/lib.rs)
shapes_rs=$(cat out/rust/src/shapes.rs)
for carrier in Tree Forest; do
  printf '%s' "$shapes_rs" \
    | grep -qF "pub struct $carrier<__KioHost:" || {
    printf 'recursive_newtype_boundary: missing exact %s nominal carrier\n' "$carrier" >&2
    exit 1
  }
  printf '%s' "$shapes_rs" \
    | grep -qF "type Facade = crate::shapes::nominal::recursiveBoundary::$carrier<__KioHost>;" || {
    printf 'recursive_newtype_boundary: %s marker does not select its exact nominal facade\n' "$carrier" >&2
    exit 1
  }
done
graft=$(printf '%s' "$lib_rs" | grep -E 'pub fn graft\(' || true)
if ! printf '%s' "$graft" | grep -qF 'crate::shapes::nominal::recursiveBoundary::Tree<__KioHost>'; then
  printf 'recursive_newtype_boundary: graft does not accept the exact Tree nominal facade\n' >&2
  exit 1
fi
if [ "$(printf '%s' "$graft" | grep -oF 'crate::shapes::nominal::recursiveBoundary::Forest<__KioHost>' | wc -l)" -lt 2 ]; then
  printf 'recursive_newtype_boundary: graft does not accept and return the exact Forest nominal facade\n' >&2
  exit 1
fi
if printf '%s' "$graft" | grep -qF '::std::rc::Rc<dyn ::std::any::Any>'; then
  printf 'recursive_newtype_boundary: graft leaked erased storage through its public facade\n' >&2
  exit 1
fi
(
  cd out/rust
  cargo check --offline --quiet
)

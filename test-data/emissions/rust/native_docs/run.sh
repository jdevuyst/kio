#!/bin/sh
# SUBJECT: Retained live-boundary docs attach as inert Rustdoc without changing generated code or host requirements.
# CONTRACT: specs/backends/rust.md, Native documentation.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.native-docs.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
python3 - <<'PY'
from pathlib import Path
path = Path("testapi/native.kio")
source = path.read_bytes()
needle = b"/// NATIVE-DOC examples stay presentation only."
assert source.count(needle) == 1
path.write_bytes(source.replace(needle, needle + b"\rCR-DOC remains commented.\xef\xbb\xbf BOM-DOC stays inert."))
PY
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

python3 - <<'PY'
from pathlib import Path
import re

root = Path("out/rust/src")
files = {p.name: p.read_text() for p in root.glob("*.rs")}
source = "\n".join(files.values())
for marker in ("EXPORT-DOC", "FIRST-SAME-DOC", "SECOND-SAME-DOC", "HOST-FN-DOC",
               "HOST-TYPE-DOC", "NEWTYPE-DOC", "OPAQUE-DOC", "GENERIC-DOC",
               "EXISTENTIAL-DOC", "NATIVE-DOC", "CR-DOC", "BOM-DOC"):
    count = len(re.findall(r"(?<![A-Z-])" + re.escape(marker) + r"(?![A-Z-])", source))
    assert count == 1, (marker, count)
assert source.count("HOST-GENERIC-DOC") == 2
for marker in ("MODULE-DOC", "PRIVATE-DOC", "UNBRIDGED-DOC", "REPLACEMENT-DOC"):
    assert marker not in source, marker
assert "[`literal_indented`]" in source, "indented code payload was rewritten"
assert "[`literal_nested`]" in source, "nested-list code payload was rewritten"
assert "fn testapi_native__archived(" in files["host.rs"], "retained negative must execute"
for filename, marker, declaration in (
    ("host.rs", "HOST-TYPE-DOC", r"    type testapi__String:"),
    ("host.rs", "HOST-FN-DOC", r"    fn testapi_io__print\("),
    ("lib.rs", "EXPORT-DOC", r"    pub fn main\("),
    ("lib.rs", "NATIVE-DOC", r"    pub fn example\("),
    ("shapes.rs", "NEWTYPE-DOC", r"        pub struct Token<"),
):
    assert re.search(r"(?m)^\s*/// " + marker + r"[^\n]*\n(?:\s*///[^\n]*\n)*(?:\s*#\[[^\n]*\n)*\n?" + declaration, files[filename]), marker
assert "::std::rc::Rc" in source and "T: 'static" in source
assert all(line == line.rstrip() for line in source.splitlines()
           if line.lstrip().startswith("///")), "doc framing has trailing whitespace"
PY

(
  cd out/rust
  if ! cargo test --doc --offline --quiet >"$scratch/rustdoc.log" 2>&1; then
    cat "$scratch/rustdoc.log" >&2
    exit 1
  fi
  RUSTDOCFLAGS="${RUSTDOCFLAGS-} -D rustdoc::broken_intra_doc_links" \
    cargo doc --no-deps --offline --quiet
)
grep -q 'running 0 tests' "$scratch/rustdoc.log"
python3 - <<'PY'
from pathlib import Path
import re
source = Path("out/rust/src/lib.rs").read_text()
pages = list(Path("out/rust/target/doc").rglob("*.html"))
relative_anchor = '<a href="UnknownKioType"><strong>relative</strong> <code>resource</code></a>'
assert any(relative_anchor in page.read_text() for page in pages), "relative resource link changed"
assert any('href="https://example.invalid/guide"' in page.read_text()
           for page in pages), "external link changed"
for field, marker, other in (("main", "FIRST-SAME-DOC", "SECOND-SAME-DOC"),
                             ("other", "SECOND-SAME-DOC", "FIRST-SAME-DOC")):
    # Discover the native type through its specified public namespace property.
    owners = re.findall(r"pub " + field + r": ([A-Za-z_][A-Za-z_0-9]*)<", source)
    assert len(owners) == 1, (field, owners)
    pages = list(Path("out/rust/target/doc").rglob("struct." + owners[0] + ".html"))
    assert len(pages) == 1, (field, pages)
    page = pages[0].read_text()
    method = re.search(r'id="method.same".*?(?=<section id="method\.|$)', page, re.S)
    assert method and marker in method[0] and other not in method[0], field
PY

# Only authored doc lines are removed from the second compiler input.
cp -R . "$scratch/plain"
python3 - "$scratch/plain" <<'PY'
from pathlib import Path
import sys
for path in Path(sys.argv[1]).rglob("*.kio"):
    path.write_bytes(b"\n".join(line for line in path.read_bytes().split(b"\n")
                               if not line.lstrip().startswith(b"///")))
PY
(
  cd "$scratch/plain"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
python3 - "$scratch/plain/out/rust" <<'PY'
from pathlib import Path
import sys

def tree(root):
    paths = [p for p in root.rglob("*") if p.is_file()
             and p.relative_to(root).parts[0] != "target"]
    return {p.relative_to(root): p.read_bytes() for p in paths
            if p.relative_to(root) != Path("Cargo.lock")}
def without_docs(data):
    return b"".join(line for line in data.splitlines(keepends=True)
                    if not line.lstrip().startswith(b"///"))
documented = tree(Path("out/rust"))
plain = tree(Path(sys.argv[1]))
assert documented.keys() == plain.keys(), (documented.keys() - plain.keys(),
                                           plain.keys() - documented.keys())
assert all(b"-DOC" not in data for data in plain.values()), "plain cached build retained authored docs"
for path in documented:
    assert without_docs(documented[path]) == without_docs(plain[path]), path
PY
printf 'native rust docs verified\n'

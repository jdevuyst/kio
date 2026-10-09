#!/bin/sh
# SUBJECT: Live boundary documentation is attached to native declarations and does not change executable artifacts.
# CONTRACT: specs/backends/haskell.md, Doc comments.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.native-docs.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
python3 - <<'PY'
from pathlib import Path
path = Path("testapi/main.kio")
source = path.read_bytes()
needle = b"/// EXPORT-DOC main."
assert source.count(needle) == 1
path.write_bytes(source.replace(needle, needle + b" big.Int.\rCR-DOC remains commentary."))
PY
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

haddock --html --odir="$scratch/docs" out/haskell/NativeDocs.hs >"$scratch/haddock.log" 2>&1 || {
  cat "$scratch/haddock.log" >&2
  exit 1
}

python3 - "$scratch" <<'PY'
from pathlib import Path
import re
import sys
scratch = Path(sys.argv[1])
documentation = "\\n".join(p.read_text() for p in (scratch / "docs").rglob("*.html"))
for marker in ["EXPORT-DOC","FIRST-SAME-DOC","SECOND-SAME-DOC","HOST-FN-DOC","HOST-GENERIC-DOC","OPAQUE-DOC","GENERIC-DOC","EXISTENTIAL-DOC","CR-DOC","HOST-TYPE-DOC"]:
    assert re.search(r"(?<![A-Z-])" + re.escape(marker) + r"(?![A-Z-])", documentation), marker
for marker in ("PRIVATE-DOC", "MODULE-DOC", "UNBRIDGED-DOC"):
    assert marker not in documentation, marker
assert "NEWTYPE-DOC" not in documentation
html = (scratch / "docs/NativeDocs.html").read_text()
def declaration(anchor):
    selected = html.split('id="' + anchor + '"', 1)
    assert len(selected) == 2, anchor
    return selected[1].split('id="', 1)[0]
for module, marker, other in (("main", "FIRST-SAME-DOC", "SECOND-SAME-DOC"),
                              ("other", "SECOND-SAME-DOC", "FIRST-SAME-DOC")):
    selected = declaration("v:export__testapi__" + module + "__same")
    assert marker in selected and other not in selected, module
assert "HOST-TYPE-DOC" in declaration("t:HostType__testapi__String")
assert "HOST-FN-DOC" in declaration("v:host__testapi__io__print")
PY

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
python3 - "$scratch/plain/out/haskell" <<'PY'
from pathlib import Path
import re
import sys
def tree(root):
    return {p.relative_to(root): p.read_text() for p in root.rglob("*.hs")}
def without_documentation(source):
    lines = []
    field_prefix = None
    for line in source.splitlines():
        if not line.strip() or line.lstrip().startswith("--"):
            continue
        prefix, marker, _ = line.partition("-- |")
        if marker and prefix.strip() in ("{", ","):
            assert field_prefix is None
            field_prefix = prefix
            continue
        if field_prefix is not None:
            assert line.startswith("    ")
            line = field_prefix + line[4:]
            field_prefix = None
        lines.append(line)
    assert field_prefix is None
    return "\n".join(lines)
documented = tree(Path("out/haskell"))
plain = tree(Path(sys.argv[1]))
assert documented and documented.keys() == plain.keys()
for path in documented:
    assert not re.search(r"[A-Z]+(?:-[A-Z]+)*-DOC", plain[path]), path
    assert without_documentation(documented[path]) == without_documentation(plain[path]), path
PY
printf 'native haskell docs verified\n'

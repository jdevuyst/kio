#!/bin/sh
# SUBJECT: Retained live-boundary docs attach to Swift public symbols without changing generated code or host requirements.
# CONTRACT: specs/backends/swift.md, Native documentation.
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
path.write_bytes(source.replace(needle, needle + b"\rCR-DOC remains commented.\x00 NUL-DOC is visible.\xef\xbb\xbf BOM-DOC stays inert."))
PY
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

python3 - <<'PY'
from pathlib import Path
import re
source = "\n".join(p.read_text() for p in Path("out/swift").glob("*.swift"))
for marker in ("EXPORT-DOC", "FIRST-SAME-DOC", "SECOND-SAME-DOC", "HOST-FN-DOC",
               "HOST-TYPE-DOC", "HOST-GENERIC-DOC", "NEWTYPE-DOC", "OPAQUE-DOC",
               "GENERIC-DOC", "EXISTENTIAL-DOC", "NATIVE-DOC", "CR-DOC", "NUL-DOC", "BOM-DOC"):
    count = len(re.findall(r"(?<![A-Z-])" + re.escape(marker) + r"(?![A-Z-])", source))
    assert count == 1, (marker, count)
for marker in ("MODULE-DOC", "PRIVATE-DOC", "UNBRIDGED-DOC", "REPLACEMENT-DOC"):
    assert marker not in source, marker
assert "[`literal_indented`]" in source, "indented code payload was rewritten"
assert "[`literal_nested`]" in source, "nested-list code payload was rewritten"
assert "```a`b" not in source, "native fence info cannot contain a backtick"
assert "func testapi_native__archived(" in source, "retained negative must execute"
assert "\x00" not in source, "native NUL warning must not be introduced"
assert all(line == line.rstrip() for line in source.splitlines()
           if line.lstrip().startswith("///")), "doc framing has trailing whitespace"
PY
mkdir -p "$scratch/symbols" "$scratch/module-cache"
swiftc -emit-module -module-name NativeDocs \
  -module-cache-path "$scratch/module-cache" \
  -emit-module-path "$scratch/NativeDocs.swiftmodule" \
  -emit-symbol-graph -emit-symbol-graph-dir "$scratch/symbols" \
  out/swift/*.swift
python3 - "$scratch/symbols/NativeDocs.symbols.json" <<'PY'
import json
import re
import sys
graph = json.load(open(sys.argv[1], encoding="utf-8"))
symbols = graph["symbols"]
def docs(symbol):
    return "\n".join(line["text"] for line in symbol.get("docComment", {}).get("lines", []))
for marker, kind, title in (
    ("HOST-TYPE-DOC", "swift.associatedtype", "testapi__String"),
    ("HOST-GENERIC-DOC", "swift.typealias", "KioHostType_testapi__Box"),
    ("HOST-FN-DOC", "swift.method", "testapi_io__print"),
    ("EXPORT-DOC", "swift.method", "main"),
    ("FIRST-SAME-DOC", "swift.method", "same"),
    ("SECOND-SAME-DOC", "swift.method", "same"),
    ("NEWTYPE-DOC", "swift.struct", "KioNewtype_testapi__Token"),
    ("OPAQUE-DOC", "swift.struct", "KioNewtype_testapi__Opaque"),
    ("GENERIC-DOC", "swift.enum", "KioNewtypeMk_testapi__Wrapper"),
    ("EXISTENTIAL-DOC", "swift.struct", "KioNewtype_testapi__Packed"),
    ("NATIVE-DOC", "swift.method", "example"),
    ("CR-DOC", "swift.method", "example"),
    ("NUL-DOC", "swift.method", "example"),
    ("BOM-DOC", "swift.method", "example"),
):
    selected = [symbol for symbol in symbols if re.search(r"(?<![A-Z-])" + re.escape(marker) + r"(?![A-Z-])", docs(symbol))]
    assert len(selected) == 1, (marker, len(selected))
    assert selected[0]["kind"]["identifier"] == kind, selected[0]
    assert title in selected[0]["names"]["title"], selected[0]
native = docs(next(symbol for symbol in symbols if "NATIVE-DOC" in docs(symbol)))
assert all(text in native for text in ("Metadata", "Snippet", "missingSymbol", "param absent"))
inert_directive = "```text\n@Metadata { @TechnologyRoot }\n```"
assert inert_directive in native, "hostile fence must retain its code payload in a valid fence"
prose = native.replace(inert_directive, "")
assert not re.search(r"(?m)^\s*[@\\]+(?:Metadata|Snippet|param)", prose), native
assert not re.search(r"(?<!`)``(?!`)", native), native
classes = {symbol["identifier"]["precise"] for symbol in symbols
           if symbol["kind"]["identifier"] == "swift.class"}
for field, marker in (("KioModule_main", "FIRST-SAME-DOC"),
                      ("KioModule_other", "SECOND-SAME-DOC")):
    properties = [symbol for symbol in symbols
                  if symbol["kind"]["identifier"] == "swift.property"
                  and symbol["names"]["title"] == field]
    assert len(properties) == 1, (field, properties)
    owners = {fragment.get("preciseIdentifier")
              for fragment in properties[0]["declarationFragments"]} & classes
    assert len(owners) == 1, (field, owners)
    method = next(symbol for symbol in symbols if marker in docs(symbol))
    assert any(relation["kind"] == "memberOf"
               and relation["source"] == method["identifier"]["precise"]
               and relation["target"] in owners
               for relation in graph["relationships"]), marker
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
python3 - "$scratch/plain/out/swift" <<'PY'
from pathlib import Path
import sys
def without_docs(data):
    result = []
    for line in data.splitlines(keepends=True):
        # This existing synthesized carrier note joins the authored doc block.
        if line.startswith(b"/// Exact ") and b" carrier for Kio newtype " in line:
            line = line[1:]
        if not line.lstrip().startswith(b"///"):
            result.append(line)
    return b"".join(result)
documented = {p.name: p.read_bytes() for p in Path("out/swift").glob("*.swift")}
plain = {p.name: p.read_bytes() for p in Path(sys.argv[1]).glob("*.swift")}
assert documented.keys() == plain.keys()
assert all(b"-DOC" not in data for data in plain.values()), "plain cached build retained authored docs"
for path in documented:
    assert without_docs(documented[path]) == without_docs(plain[path]), path
PY
printf 'native swift docs verified\n'

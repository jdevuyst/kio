import ast
import pathlib
import re
import sys

if sys.argv[1] in ("--strip-source", "--edit-source"):
    for path in pathlib.Path(sys.argv[2]).rglob("*.kio"):
        if "out" not in path.parts:
            text = path.read_text(encoding="utf-8")
            path.write_text(text.replace("EXPORT-DOC", "EXPORT-EDIT-DOC") if sys.argv[1] == "--edit-source" else "".join(line for line in text.splitlines(keepends=True) if not line.lstrip().startswith("///")), encoding="utf-8")
    sys.exit(0)

root = pathlib.Path(sys.argv[1])
ast.parse((root / "native_docs.py").read_text(encoding="utf-8"))
paths = sorted((root / "native_docs").glob("*.pyi"))
trees = {path.name: ast.parse(path.read_text(encoding="utf-8")) for path in paths}
assert trees, "no stub shards were emitted"
export_marker = "EXPORT-EDIT-DOC" if len(sys.argv) > 2 and sys.argv[2] == "--edited" else "EXPORT-DOC"
markers = ("HOST-GENERIC-DOC", "HOST-FN-DOC", export_marker, "FIRST-SAME-DOC", "SECOND-SAME-DOC", "NEWTYPE-DOC", "OPAQUE-DOC", "GENERIC-DOC", "EXISTENTIAL-DOC")
def has(text, marker):
    return re.search(r"(?<![A-Z-])" + marker + r"(?![A-Z-])", text) is not None

if len(sys.argv) > 3 and sys.argv[3] == "--absent":
    source = "\n".join(path.read_text(encoding="utf-8") for path in paths)
    for marker in (*markers, "EXPORT-EDIT-DOC"):
        assert not has(source, marker), f"cached stale documentation: {marker}"
    class WithoutDocs(ast.NodeTransformer):
        def clean(self, node):
            self.generic_visit(node)
            if node.body and isinstance(node.body[0], ast.Expr) and isinstance(node.body[0].value, ast.Constant) and isinstance(node.body[0].value.value, str):
                del node.body[0]
            return node
        visit_Module = clean
        visit_ClassDef = clean
        visit_FunctionDef = clean
    previous = pathlib.Path(sys.argv[2])
    assert set(trees) == {path.name for path in previous.glob("*.pyi")}
    for name, tree in trees.items():
        before = ast.parse((previous / name).read_text(encoding="utf-8"))
        assert ast.dump(WithoutDocs().visit(tree)) == ast.dump(WithoutDocs().visit(before)), f"documentation changed declarations: {name}"
    sys.exit(0)

classes = {node.name: node for tree in trees.values() for node in ast.walk(tree) if isinstance(node, ast.ClassDef)}
methods = [node for tree in trees.values() for node in ast.walk(tree) if isinstance(node, ast.FunctionDef)]
docs = [ast.get_docstring(node, clean=False) for node in [*classes.values(), *methods] if ast.get_docstring(node, clean=False)]
for marker in markers:
    assert any(has(doc, marker) for doc in docs), f"missing {marker}"
for marker in ("PRIVATE-DOC", "MODULE-DOC", "UNBRIDGED-DOC"):
    assert not any(has(doc, marker) for doc in docs), f"leaked {marker}"

def child(node, name):
    field = next(item for item in node.body if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name) and item.target.id == name)
    annotation = field.annotation.value if isinstance(field.annotation, ast.Subscript) else field.annotation
    assert isinstance(annotation, ast.Name)
    return classes[annotation.id]

api = child(classes["NativeDocs"], "testapi")
main_module = child(api, "KioModule_main")
other_module = child(api, "KioModule_other")
def method(node, name):
    return next(item for item in node.body if isinstance(item, ast.FunctionDef) and item.name == name)
for node, expected, wrong in (
    (method(main_module, "same"), "FIRST-SAME-DOC", "SECOND-SAME-DOC"),
    (method(other_module, "same"), "SECOND-SAME-DOC", "FIRST-SAME-DOC"),
):
    doc = ast.get_docstring(node, clean=False)
    assert doc and has(doc, expected) and not has(doc, wrong), f"wrong identity: {expected}"
host = next(ast.get_docstring(node, clean=False) for node in methods if node.name == "print" and has(ast.get_docstring(node, clean=False) or "", "HOST-FN-DOC"))
assert '*/' in host and '"""' in host and '\\' in host and 'α' in host and '@deprecated' in host
main = ast.get_docstring(method(main_module, "main"), clean=False)
assert main and has(main, export_marker) and "bold" in main and "emphasis" in main and "https://example.invalid/guide" in main
assert not has(main, "EXPORT-DOC" if export_marker == "EXPORT-EDIT-DOC" else "EXPORT-EDIT-DOC"), "stale edited documentation"
tick = chr(96)
assert "[" + tick + "literal_code" + tick + "]" in main and "[" + tick + "tilde_code" + tick + "]" in main

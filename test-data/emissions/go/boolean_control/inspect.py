import pathlib
import re
import sys

source = pathlib.Path(sys.argv[1]).read_text()


def body(name):
    matches = list(re.finditer(r"^func \(pkg [^\n]+\) (\w+_" + name + r")\([^\n]*\) any \{\n", source, re.M))
    assert len(matches) == 1, (name, "missing or ambiguous subject")
    start = matches[0].end()
    end = source.index("\n}\n", start)
    return source[start:end]


for name in ["inline_calls", "inline_blocks", "inline_operators", "inline_effect"]:
    code = body(name)
    assert not re.search(r"\bfunc\s*\([^)]*\)[^\n{}]*\{", code), (name, "inline branch closure survived")
    assert "if " in code or " && " in code, (name, "native control absent")

assert "if " in body("inline_effect"), "effectful RHS needs branch-local statements"
assert re.search(r"right[^\n]*\(\)", body("opaque")), "opaque callback invocation disappeared"
staged = body("staged")


def host_call(name):
    binding = re.search(r"(\w+) = pkg\.host\.Main__" + name + r"\b", staged)
    assert binding, (name, "host operand absent")
    invocation = re.search(r"\b" + re.escape(binding[1]) + r"\(", staged[binding.end():])
    assert invocation, (name, "host operand not invoked")
    return binding.end() + invocation.start()


assert host_call("condition") < host_call("factory") < staged.index("if "), "operand factories moved across dispatch"

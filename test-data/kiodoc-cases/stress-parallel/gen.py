#!/usr/bin/env python3
"""Generate a synthetic kiodoc corpus for the wall-clock sanity check.

Writes a docs-only Kio package: a `pkg.pkg.kio` whose
`build { ... }` block declares `docs { md "input" }`, plus
`input/all.md` with one shared harness and N trivial snippets that
each typecheck cleanly. Used for the `02-per-snippet-parallelism`
session's `kio doc check` wall-clock comparison across
`RAYON_NUM_THREADS` settings. Not part of the goldens corpus — see
`README.md` in this directory.
"""

import os
import sys


def main() -> int:
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 200
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, "input", "all.md")
    os.makedirs(os.path.dirname(out), exist_ok=True)

    # The generated corpus is itself a docs-only package: `kio doc
    # check` reads this package file's `build { ... }` block, finds
    # `docs` `md "input"`, and walks the markdown tree there.
    with open(os.path.join(here, "pkg.pkg.kio"), "w") as f:
        f.write(
            "package pkg;\n\n"
            'build {\n  cache ();\n\n  docs {\n    md "input";\n  };\n}\n'
        )

    tick3 = "`" * 3
    lines = []
    lines.append("# Stress fixture\n\n")
    lines.append(
        "Synthetic corpus of trivially-valid kiodoc snippets used to "
        "compare wall-clock time across `RAYON_NUM_THREADS` settings. "
        "Not a byte-comparison golden — see this directory's README.\n\n"
    )

    # One shared harness — every snippet references it via `{@h}`.
    # Three host types + three host functions keep the per-snippet typer
    # work above the rayon per-task overhead floor.
    lines.append(tick3 + 'kio {harness=h placeholder="__INSERT_CODE_HERE__"}\n')
    lines.append("bridge {\n")
    lines.append("  kiodoc;\n")
    lines.append("}\n")
    lines.append("\n")
    lines.append("module kiodoc;\n")
    lines.append("\n")
    lines.append("host type S role(str);\n")
    lines.append("host type N role(i32);\n")
    lines.append("host type B role(bool);\n")
    lines.append("host fn id_s(value: S) -> S;\n")
    lines.append("host fn id_n(value: N) -> N;\n")
    lines.append("host fn id_b(value: B) -> B;\n")
    lines.append("\n")
    lines.append("__INSERT_CODE_HERE__\n")
    lines.append(tick3 + "\n\n")

    for i in range(n):
        lines.append(f"### Snippet {i}\n\n")
        lines.append(tick3 + "kio {@h}\n")
        lines.append(f"pub fn item_{i}(s: S, n: N, b: B) -> (S & N & B) {{\n")
        lines.append("    (id_s(s), id_n(n), id_b(b))\n")
        lines.append("}\n")
        lines.append(tick3 + "\n\n")

    with open(out, "w") as f:
        f.writelines(lines)
    print(f"wrote {out} with {n} snippets")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

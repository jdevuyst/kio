"""Copy the archived sources into a new external fixture directory; execute no tools."""

import argparse
import json
import os
import pathlib
import shutil

ROOT = pathlib.Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    temp_root = pathlib.Path(os.environ["TMPDIR"]).resolve(strict=True)
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        parser.error("output must be new")
    parent = output.parent.resolve(strict=True)
    if not parent.is_relative_to(temp_root):
        parser.error("output parent must be below TMPDIR")
    if parent.is_relative_to(ROOT):
        parser.error("output must be outside the source archive")
    output = parent / output.name
    output.mkdir()
    shutil.copy2(ROOT / "prepare.py", output / "prepare.py")
    shutil.copy2(ROOT / "presets.json", output / "presets.json")
    shutil.copytree(ROOT / "primary", output / "primary",
                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    for source in (output / "primary").rglob("*.in"):
        source.with_suffix("").write_bytes(source.read_bytes())
    template = output / "primary/trusted-cell.template.json"
    doc = json.loads(template.read_text())
    doc["env"]["TMPDIR"] = str(temp_root)
    template.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
    application = output / "primary/application"
    for arm in ("B", "T", "C", "S", "P"):
        repeated = application / "repeat-source" / arm
        repeated.mkdir(parents=True)
        shutil.copy2(application / "repeat-driver.rs", repeated / "driver.rs")
        shutil.copy2(application / "order-book.expected.stdout",
                     repeated / "order-book.expected.stdout")
    print(json.dumps({"fixture": str(output), "compiler_or_host_runs": 0,
                      "status": "sources copied; campaign receipts absent"}))


if __name__ == "__main__":
    main()

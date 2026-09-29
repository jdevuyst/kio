# stress-parallel — wall-clock sanity fixture

This directory is not a golden-test case (no `run.sh`, no
`expected.*`, no `00_/70_` category prefix). The `ci/run-tests.sh`
walker only treats directories that contain `run.sh` as cases, so
having a `README.md` and a generator script here doesn't trip it.

Generate the corpus — `gen.py` writes a `pkg.pkg.kio` (with a
`build { ... }` block) plus the markdown tree, so this directory
becomes a docs-only Kio package:

```sh
python3 test-data/kiodoc-cases/stress-parallel/gen.py 200
```

Run `kio doc check` over the generated package and compare wall
time across thread counts. `kio doc check` reads the package
file's build block, so it must run from inside the package
directory:

```sh
cd test-data/kiodoc-cases/stress-parallel
time RAYON_NUM_THREADS=1 ../../../kio-rs/target/release/kio doc check
time RAYON_NUM_THREADS=8 ../../../kio-rs/target/release/kio doc check
```

The eight-thread run should finish noticeably faster on a
multi-core machine; that's the wall-clock sanity check this
fixture exists to demonstrate. The corpus is deliberately not
committed to the goldens corpus: each snippet is trivially valid
(`pub fn item_N(...)` whose body returns its arguments) and the
test would just measure the typer's tiny constant overhead, not
the per-snippet path.

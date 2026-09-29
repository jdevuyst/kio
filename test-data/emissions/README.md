# Emission cases

`test-data/emissions/` holds backend-specific evidence about generated host artifacts. It has exactly two subjects:

- a public host interface that independently authored host code can use; or
- a durable generated-artifact fact required by a backend spec or justified by a recorded measurement.

Language, diagnostic, and runtime semantics stay in cross-implementation goldens. Emission cases supplement that coverage; they never satisfy a runtime cell in `audit-backend-completeness`.

## Layout

Cases use one backend bucket and one case directory:

```text
test-data/emissions/<backend>/<case>/
  HOST_INTERFACE | ARTIFACT_SHAPE
  workdir/
    <package>.pkg.kio
    ... Kio modules ...
  host/                         # HOST_INTERFACE only
  run.sh
  expected.stdout
  expected.exit
  expected.stderr.ignore | expected.stderr.grep | expected.stderr
```

The backend buckets are `js`, `ts`, `python`, `java`, `rust`, `go`, `swift`, and `haskell`. The filesystem is the registry: do not add a separate marker or case catalogue.

Every case satisfies all of these structural rules:

- It is a direct child of `test-data/emissions/<backend>/`.
- It contains exactly one empty subject marker: `HOST_INTERFACE` or `ARTIFACT_SHAPE`.
- `workdir/` contains exactly one root `*.pkg.kio` file, whose `build { ... }` block contains exactly one target, matching the backend bucket. A genuinely multi-artifact host-interface case may also contain nested packages; each nested manifest declares that same sole target, and the root package is one of the artifacts actually built rather than a routing-only dummy.
- `run.sh` is the only execution file. There is no `run.args`, `run.test-only`, `KNOWN_FAILING`, or `oracles/` directory.
- `expected.exit` is exactly `0`, with or without a final newline, and exactly one stderr-policy file is present.
- The case is self-contained and portable. It names no checkout, home, scratch, or machine-specific path.

Each `run.sh` has a non-empty leading `# SUBJECT: ...` comment after its shebang, stating the generated-host fact it proves. A `# CONTRACT: ...` line may name the backend-spec heading or recorded measurement that makes the assertion durable. The script uses `$KIO_BIN` for the selected compiler and `$KIO_TARGET` for the target, preserves the harness-provided `PATH`, and invokes native compilers by bare command name so compiler admission remains in force. It does not invoke `$KIO_RUNNER`; the emissions orchestrator supplies a rejecting runner as a tripwire because the case owns its host/artifact assertion.

The script treats the checked-in `workdir/` as immutable input. It creates a case-local scratch directory below the harness-provided `TMPDIR`, installs a trap to remove it, copies `workdir/` there, changes into the copied workdir, and runs every `$KIO_BIN build "$KIO_TARGET"` from that copy. It never changes into or builds the tracked `workdir/` directly. Use this shape, with a descriptive portable prefix:

```sh
scratch=$(mktemp -d "${TMPDIR:?}/emissions.case.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"$KIO_BIN" build "$KIO_TARGET"
```

The orchestrator also pins `--cache-base` below its own temporary root. Emission builds therefore leave neither `out/` nor a Kio cache in the checked-in case and do not use the machine-shared test-runner artifact cache.

## `HOST_INTERFACE`

A `HOST_INTERFACE` case proves that the published generated facade is usable by an ordinary host author.

- `host/` is required. Normally it contains fixed host-language source authored from `specs/backends/<backend>.md`, not from the artifact produced during the run. A capacity case whose host is mechanically enormous may instead keep fixed public-spec parameters in `host/` and deterministically generate the repeated source from only those parameters; the generator never reads the emitted artifact, and the exception must reduce rather than hide reviewable contract content.
- The host uses the documented public loading, host-record, namespace, type, and package API. It does not reach a private symbol.
- Case code does not read, copy, grep, patch, or parse generated files to discover names, signatures, shapes, fixtures, or compiler arguments. The host compiler/runtime may import, compile, link, or load the generated artifact in the ordinary documented way.
- Changing the emitter's public facade incompatibly makes the unchanged host fail to compile or run. That independence is the oracle.

Use this marker for generated-host ABI, public selector collisions, namespace/package coexistence at the host source level, public type relationships, and other host-author operations that a fixed runner protocol cannot independently demonstrate.

## `ARTIFACT_SHAPE`

An `ARTIFACT_SHAPE` case proves a durable property of generated files. `host/` is forbidden.

The assertion must follow from one of:

- a public backend-spec contract such as output paths, required manifests, language modes, or named public files; or
- a portable artifact proxy retained with the recorded measurement that justified an optimization or compiler-resource fix.

The script may inspect the generated tree to assert that fact. It does not pin incidental formatting, private helper names, declaration order, or another implementation detail merely because it is easy to grep. Private invariants that need no filesystem belong in Rust unit or mutation tests. If an old artifact assertion has no durable contract or measurement role, delete it instead of migrating it.

## Boundary with goldens and Kio'

Golden-owned files never read, copy, grep, patch, import, or native-compile generated host-backend files. They pass output directories opaquely to fixed ordinary runner protocols for cross-implementation runtime checks.

Kio' is different: it is a specified backend-neutral phase artifact. Goldens may read or assemble Kio' when its grammar, round trip, verifier, evaluator, or dynamic-load boundary is the subject, and harness-owned phase checks remain valid. Kio' does not belong in this host-backend corpus.

## Running

The direct orchestrator runs all cases by default:

```sh
sh ci/checks/orchestrators/emissions-tests.sh --impls=FULL_IMPL_MATRIX
```

Filters match `<backend>/<case>` and follow `--`:

```sh
sh ci/checks/orchestrators/emissions-tests.sh \
  --impls=FULL_IMPL_MATRIX -- '^rust/public_facade$'
```

`--sample-cases` uses a default cap of one case per available backend. `--case-count=<N>` changes that per-backend cap for the direct orchestrator, `--case-seed=<S>` reproduces the draw, and `--all-cases` restores the full corpus. At the top level use `--case-coverage=emissions:<policy>`. Local broad runs leave emissions at their default of all cases.

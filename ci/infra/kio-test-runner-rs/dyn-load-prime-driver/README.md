# dyn-load-prime-driver

The host package the `kio-test-runner-dyn-load-prime` runner drives.

The dyn-load-prime runner interprets each golden's Kio' (Prime) image through
`dyn_load_prime`'s `prime_eval` evaluator. Architecturally the runner is a
`dyn_load_prime` *host*: it drives a small pure-Kio driver package that vendors
`dyn_load_prime` and loads a guest image at runtime — the same
`load_package` → call flow `test-data/poc/dyn_load_prime/workdir/testapi/main.kio`
proves, generalized to follow the exact shared protocol's load-only,
construct-only, module-qualified-main, or export-script execution mode.

## What's here

- `driver.kio` — the driver module. Its `main` reads a guest's emitted
  Kio' image text from the `read_guest_image` host capability — the whole
  `kio build kio-prime` output tree concatenated, host modules and the
  `.pkg.kio` manifest included — `load_package`s it, and dispatches
  on the case's validated protocol name and protocol-derived execution mode:
  `compile-only` returns after loading, `construct-only` instantiates the exact
  empty-host package without invoking an export, every main protocol
  instantiates its selected exact host contract and invokes `main` in the
  protocol's exact declaring module through the loaded surface, and each
  supported export protocol drives the loaded surface exactly as that
  protocol's compiled-runner driver does,
  byte-for-byte. The guest's own host
  effects (`print`, arithmetic, …) flow through `dyn_load_prime`'s
  evaluator and are serviced by the package's `testapi/*` host fns plus
  the driver's per-protocol capability arms.
- `dyn_load_prime_driver.pkg.kio` — the driver package manifest; its
  `bridge { ... }` block exposes the vendored loader's modules, the
  `testapi` capability surface, and the `driver` module itself.
- `build-driver.sh` — assembles the live `test-data/poc/dyn_load_prime/workdir/` package (the
  single source of truth — no vendored fork) plus the two files above
  in an invocation-local staging directory and runs `kio build js`. It
  publishes the result as an immutable, input-addressed artifact and prints
  that stable driver JS path.

## How it's used

`ci/checks/orchestrators/golden-tests.sh` builds the driver once via
`build-driver.sh`, then exports
`KIO_DYN_LOAD_PRIME_DRIVER_JS` to the runner. Per case, the harness builds the
case to the `kio-prime` target and invokes the runner with that output
directory; the runner reads the guest image text from it, injects it into
the driver's `read_guest_image`, evaluates the driver under QuickJS, and
captures the guest's observable behaviour. The golden harness's
stdout / exit diff against `expected.*` is the interpreter-vs-compile-and-
run differential.

The publication directory is shared by golden orchestrators in one checkout.
The key covers the compiler, assembly builder, and copied package inputs.
Concurrent misses use separate staging directories and publish complete files
atomically; neither a cache hit nor a rebuild mutates a previously returned
path.

The runner backs the driver package's host fns with the same protocol-defined
capability semantics as the compiled runners. It also projects the selected
protocol's exact host-type fixtures, function descriptors, and application
groups into the driver; those values come from the shared structured protocol,
not by inspecting emitted guest source.

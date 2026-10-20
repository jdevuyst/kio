# Call-stack experiment source archive

This directory preserves the sources used by the bounded B/T/C/S/P study.
The completed results, interpretation and decision to adopt none of the four
stages are recorded in [the evidence record](../../evidence/call-stack-safety.md).
Archiving these sources does not rerun or revalidate those measurements.

[presets.json](presets.json) retains the exact five revisions, per-cell inputs,
repetition counts, profiles, schedules and campaign bounds. B is released 0.2.3;
T, C, S and P are cumulative experimental revisions. The sources are an archive,
not a CI gate or a general benchmark framework.

## What is preserved

- `primary/workload/`: the common scalar Kio package and module.
- `primary/drivers/`: public Python, JavaScript/QuickJS and Rust hosts, including
  the isolated rquickjs launcher.
  Its exact 19-package dependency closure is retained in `Cargo.lock.in`.
- `primary/targeted/`: Go B/T and C/S/P adapters, Java host, strict TypeScript
  declaration consumers and their package target manifest.
- `primary/swift-targeted/`: two independent packages and baseline/staged hosts.
  `primary/haskell-targeted/`: callback and explicitly host-driven IO-bind hosts.
- `primary/oracle.py` and each targeted `check.py`: independent arithmetic,
  effect-order, owner-trace and observable root-release checks.
- `prepare.py`: source identity, source inventory and balanced scheduling code.
  `primary/collect.py`: the original bounded Linux process collector,
  literal argv execution, output/time limits, failure retention and owned drain.
- `primary/runtime/`, `primary/runtime-pilot/`, `primary/depth/`: completed
  campaign collection/analysis reference code, including suppression of deeper
  diagnostics after the first failure in an arm/backend/mode.
- `primary/application/`: repeated-entry Rust adapter, complete 2,509-byte oracle,
  preparation/collection references and matched analysis.
- `primary/compiler-cost-secondary/`: bounded compiler panel and analysis.
  `primary/python-smoke/run.py` retains the process helper those controllers use.

[source-map.json](source-map.json) binds each imported source to its original
SHA-256. Directory names were normalized; the primary checker was renamed
from `inspect.py` to `oracle.py` to avoid shadowing Python's standard library.
Path bindings, source inventory names and imports were adjusted; fixture Cargo
manifests gained the repository's dual-license metadata. Timing bodies, host
adapters and oracle arithmetic are retained. The repeated application adapter
is kept once; materialization reuses it for each arm.

Private receipts, raw results, generated backend output, executable artifacts,
caches and repeated source trees are omitted. The one-off calibration,
receipt-harvesting, build-extraction and preparation campaign scripts are omitted.
The reference controllers require independently prepared manifests and receipts:
those inputs are deliberately absent. Their source shows the original bounded
method; copying this directory does not reproduce a completed campaign.

## Cheap reproducibility checks

Use Python 3.12 or newer, with assertions enabled and bytecode writes disabled.
Set `TMPDIR` to an existing writable directory on a filesystem with adequate
space. The source/oracle checks use Python's standard library. The collector
uses Linux process groups, waitid and Linux resource accounting; its synthetic
selfcheck is Linux-only. No macOS or Windows collector evidence is claimed.

From the repository root:

```sh
python3 -B reports/call-stack-safety/prepare.py selfcheck
python3 -B reports/call-stack-safety/primary/oracle.py selfcheck
python3 -B reports/call-stack-safety/primary/targeted/check.py selfcheck
python3 -B reports/call-stack-safety/primary/swift-targeted/check.py selfcheck
python3 -B reports/call-stack-safety/primary/haskell-targeted/check.py selfcheck
python3 -B reports/call-stack-safety/primary/collect.py selfcheck
```

The last command runs only fixed synthetic Python subprocesses to exercise
timeout, output limit, literal argv and descendant cleanup. These checks execute
neither Kio nor a host workload and establish no performance result.

To render buildable fixture filenames into a new external directory, choose
an absent path whose parent exists below `TMPDIR`:

```sh
python3 -B reports/call-stack-safety/materialize.py "$TMPDIR/call-stack-fixture"
```

The materializer executes no compilers and downloads nothing. `Cargo.toml.in`
and `go.mod.in` become ordinary manifests only in that external fixture.
The rendered trusted-cell template stays marked `incomplete-do-not-execute`.
Generated files, manifests and results belong in the external fixture.

## Repeating a bounded comparison

Fresh reproduction needs explicit tool preparation and a new frozen execution
plan. Use one immutable compiler per exact revision in `presets.json`, built by
the same procedure and profile; record source trees, binaries, drivers, workload
inventories, actual tool versions, runtime configuration and limits. Preserve
configured compiler caches. The pinned repository tool configuration at P
identifies intended tools, rather than authenticating a fresh installation.
The isolated QuickJS launcher uses rquickjs exactly 0.11.0 and Cargo's release
opt-level 3. The Rust scalar package/driver use opt-level 2; the preserved Cargo
template expresses that profile. Keep the rendered QuickJS lockfile and use
`cargo build --locked --release` for its isolated launcher. Fixture crates use direct Cargo outside the
repository workspace.

Emit identical scalar sources separately with each arm's compiler using
`kio build js`, `kio build python` or `kio build rust` in
`primary/workload/`; use the corresponding targeted package for other hosts.
Compile native adapters before runtime timing. Go's `kio_baseline` build tag selects
B/T and `kio_staged` selects C/S/P. Swift B/T native callback hosts and C/S/P
`KioFn` hosts use the matching source; C/S/P's two packages link one compatible
`KioRuntime`. Java uses its matching emitted `Shapes.Fn1` API. TypeScript
checks the matching baseline/staged consumer with strict mode, while
`targeted/check.py compare-js JS_FILE TS_JS_FILE` checks emitted runtime bytes.

For example, after Python emission in an external fixture, a single firing
witness uses the driver's explicit public CLI:

```sh
python3 -B primary/drivers/python.py primary/workload/out/python/bench.py B pending 3 1 7 0 witness
```

Its JSON checksum is 17 and trace is `[0,1,2,3]`. The isolated QuickJS launcher
accepts `PACKAGE_JS DRIVER_JS CONFIG_JSON_FILE`, with config shaped like
`primary/example-input.json`. Rust and targeted native hosts accept
`ARM MODE DEPTH REPEAT SEED WARMUP PURPOSE`. Run firing witnesses before timing;
do not replace them with a checksum-only shallow test.

For a timed scalar cell, copy `primary/trusted-cell.template.json` to a fresh
plan and fill its explicit paths, file identities, input, environment, profile,
source commits/trees and receipt hashes. The collector rejects incomplete
templates and verifies literal command/file identities before and after each
sample. Review the resulting concrete plan and hash it before running:

```sh
python3 -B primary/collect.py check-trusted CELL_JSON
python3 -B primary/collect.py run-reviewed CELL_JSON --reviewed-sha256 SHA256 --output NEW_OUTPUT --repo REPOSITORY
```

`NEW_OUTPUT` must be new and below `TMPDIR`; these are runtime commands, not
compiler preparation. Keep failures and censored rows, and never calculate a
speed ratio for a failed arm. The retained controllers additionally freeze the
whole campaign and enforce dispatch deadlines. They require recreated receipts,
not substitutions that bypass identity checks. `runtime/run_final.py` and
`application/run_application.py` take an explicit `--repo`;
`depth/run_depth.py` takes the reviewed helper hash explicitly. The repeated
application preparation reference takes
`KIO_DEBUG_CALL_STACK_COMPILER_SOURCE` as its compiler/scheduler source checkout.

For compiler cost, recover the released B elab and order-book directories
named by `presets.json` with their relative materialized dependencies intact.
Use the same source seed for every arm, copy it fresh for each command and run
the preserved check/build command forms with `--no-cache`. The original panel
has three rotating matched blocks, not complete five-position balance.
Native artifact preparation is a separate phase from this compiler panel.

The application process rows use public runners, fixed corpus inputs and their
complete `expected.stdout`. The order-book source is released B. Control source
revisions are separately pinned in `presets.json`; do not substitute each arm's
own corpus version. The repeated Rust adapter links the matching order-book rlib,
uses a memory print sink, reconstructs state every entry, and checks all 2,509
bytes on every repeat. Its measured preset is 1,000 repeats with zero warmup.
The original preparation controller also verifies the original driver and rlib
receipts; native artifacts must be prepared separately.

## Reading the analysis references

The analysis files consume completed collector layouts with original schemas,
source identities and success checks. They retain assertions specific to the
completed study, including its stack observation, completion hash and sample
counts. They cannot analyze an arbitrary fresh campaign unchanged: recreate
and review its explicit metadata and update any study-specific attestation for
the new data before treating an analysis as a fresh result. No raw study data
or private receipt is required for the cheap source/oracle checks above.

Invocation time excludes package setup, chain construction, warmup and root
release. `total_phases_ns` sums invocation and those four intervals, excluding gaps; outer wall
time is separate. Matched ratios compare arms in the same block, and displayed
min/max ranges are observations, not confidence intervals. The six combined
pilot/continuation views reuse observations and are not extra replications.

Depth 10,000 is the ladder's endpoint, not proof of constant space or an
unbounded guarantee. No stack limit, GC policy or runtime limit was raised to
turn a failure into a pass. Root-slot clearing is not capture-destruction or
heap-collection evidence. QuickJS is not Node/V8, fixed Java warmup is not
proof of steady JIT state, and Haskell host IO-bind controls are not Kio
`do`/`bind` coverage. Application process cost includes loading/startup/host/IO
and cannot be pooled with isolated release-launcher invocation time.

The depth presets distinguish the initial backend/mode roster from the separately
bounded extension: 56 surviving keys at 4,096/10,000 and five Swift callback-tail
keys at 1,024/4,096/10,000, at most 127 requests. All 24 previously failed keys
are excluded; no failed probe is retried.

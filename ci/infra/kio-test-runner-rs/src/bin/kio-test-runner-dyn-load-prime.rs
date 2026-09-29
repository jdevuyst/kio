//! `kio-test-runner-dyn-load-prime` — runs a golden's Kio' (Prime) image
//! through `dyn_load_prime`'s `prime_eval` interpreter and reports the
//! observable behaviour (stdout / exit) as the differential against
//! compile-and-run.
//!
//! ## What it is
//!
//! Architecturally this runner is a `dyn_load_prime` **host**: it drives the
//! AOT-compiled dyn-load-prime driver package (`dyn_load_prime` + a `driver`
//! module, built once to JS by
//! `ci/infra/kio-test-runner-rs/dyn-load-prime-driver/build-driver.sh`). The
//! driver's `main` reads a guest's emitted Kio' image text from the
//! `read_guest_image` host capability and loads it. The selected shared runner
//! protocol determines whether the driver stops after loading, instantiates
//! the selected exact host without invoking an export, invokes `main` in its
//! exact declaring module, or runs an export-surface script. The
//! guest's own host effects (`print`, arithmetic, …) flow through
//! `dyn_load_prime`'s evaluator and are serviced by the driver package's
//! `testapi/*` host fns, which this runner backs with the **same
//! canonical bodies** the js / rust runners use. So driving a guest through
//! the interpreter produces the same observable behaviour as compile-and-run,
//! and the golden harness's stdout / exit diff is
//! the interpreter-vs-compile-and-run differential. A disagreement
//! surfaces as the harness's expected-vs-actual diff (both observed
//! behaviours printed).
//!
//! ## The pipeline
//!
//! 1. The harness builds the case to the `kio-prime` target, writing the
//!    guest's emitted Kio' module files under `out/kio-prime/`, then
//!    invokes this runner with that directory.
//! 2. The runner reads `KIO_DYN_LOAD_PRIME_DRIVER_JS` — the path to the
//!    once-compiled driver JS module the orchestrator built up front —
//!    and the guest image from the image directory: the whole emitted tree's
//!    `.kio` text concatenated, including every regular module, every host
//!    module, and the `.pkg.kio` manifest. The whole-package loader scans the
//!    concatenation and resolves cross-module references. `compile-only`
//!    stops there. Loading checks structural, import, host-contract, and
//!    export-contract edges but trusts function-body typing. `construct-only`
//!    instantiates the exact empty host without invoking an export. Every main
//!    protocol instantiates its selected exact host contract and invokes
//!    `main` in the protocol's exact declaring module through the loaded
//!    surface; export drivers operate on that surface directly.
//! 3. It evaluates the driver module under rquickjs, builds the driver's
//!    driver host record with the guest image and selected protocol projection
//!    injected into
//!    `read_guest_image`, calls the driver's branded `create<Handle>`
//!    factory (`createDynLoadPrimeDriver`, derived from the artifact's
//!    effective namespace), and invokes the driver's `main`. The guest's `print`
//!    reaches this process's stdout.
//!
//! The runner reads no `kio build` output to learn an interface. The driver's
//! own host interface is fixed (`dyn_load_prime`'s `testapi` surface plus the
//! driver callbacks), while the harness-selected `RunnerProtocol` projects
//! the guest's exact host types, descriptors, and application groups into that
//! driver. The per-case inputs are therefore the guest image text and its
//! protocol selection; emitted text is read only as data to interpret, never
//! to reconstruct a host contract.
//!
//! Exits 0 after the driver runs normally, 1 on a runner failure (a JS
//! exception, a missing driver, an unreadable image); 2 is the CLI usage tier
//! per `specs/exit-codes.md`. Guest failures are rendered into stdout for the
//! harness's differential comparison.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

use rquickjs::function::Func;
use rquickjs::{CatchResultExt, Context, Ctx, Function, Module, Object, Runtime, Value};

#[path = "../shared/artifact_identity.rs"]
mod artifact_identity;
#[cfg(feature = "rust")]
#[path = "../shared/host_api.rs"]
mod host_api;
#[path = "../shared/protocol.rs"]
mod protocol;

use artifact_identity::{ArtifactIdentityArgs, pascal_case};
use protocol::{ExportDriver, HostTypeFixture, ProtocolExecution, RoleFixture, RunnerProtocol};

const EXIT_USAGE: i32 = 2;
const EXIT_RUNTIME_FAILURE: i32 = 1;

const USAGE: &str = "\
Usage: kio-test-runner-dyn-load-prime [--protocol <name>] <image-dir>

Run a golden's Kio' (Prime) image through the once-compiled dyn_load_prime
driver package (path in KIO_DYN_LOAD_PRIME_DRIVER_JS) and report its
observable behaviour (stdout / exit). The driver loads the guest image from
<image-dir>. The selected shared runner protocol fixes whether the image is
only loaded (`compile-only`), instantiated against the selected exact host
without invoking an export (`construct-only`), driven through the exact
module-qualified `main`, or run through an export driver. Guest host effects
reach this process's streams.

Arguments:
  <image-dir>    Directory of a `kio build kio-prime` emit (the guest's
                 Kio' module files), e.g. `out/kio-prime/`.

Environment:
  KIO_DYN_LOAD_PRIME_DRIVER_JS   REQUIRED. Path to the compiled driver JS
                             module (built by the orchestrator via
                             dyn-load-prime-driver/build-driver.sh).

Options:
  --package-name <name>
                 Kio guest-package name supplied by the corpus harness.
  --protocol <name>
                 Exact shared runner protocol. Defaults to `empty-main`,
                 which instantiates the exact empty host and invokes `main`
                 in module `main`; `compile-only` only loads the image;
                 `construct-only` instantiates but invokes no export.
                 See the runner README for the protocol catalogue.
  -h, --help     Show this help and exit.
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    // QuickJS executes bytecode iteratively in C, so the embedded engine
    // itself doesn't blow the Rust stack; we still run on a dedicated
    // worker thread with a generous stack for deep Rust frames around the
    // FFI boundary, matching the JS runner.
    let exit_code = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || run(&args))
        .expect("test-runner worker thread")
        .join()
        .expect("worker thread panicked");
    process::exit(exit_code);
}

fn run(args: &[String]) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    let mut protocol = RunnerProtocol::default();
    let mut identity_args = ArtifactIdentityArgs::default();
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match identity_args.consume(a, &mut iter) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_USAGE;
            }
        }
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            // The shared protocol registry is the accepted-name and complete
            // execution-contract authority. The Kio driver receives only the
            // canonical name plus the execution mode derived below.
            "--protocol" => match iter.next() {
                Some(v) => match RunnerProtocol::parse(v) {
                    Ok(parsed) => {
                        protocol = parsed;
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                },
                None => {
                    eprintln!("error: {a} requires a value");
                    return EXIT_USAGE;
                }
            },
            s if s.starts_with("--protocol=") => {
                protocol = match RunnerProtocol::parse(&s["--protocol=".len()..]) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
            }
            s if s.starts_with("--") => {
                eprintln!("error: unknown option: {s}");
                return EXIT_USAGE;
            }
            s => positional.push(s),
        }
    }

    let image_dir = match positional.as_slice() {
        [d] => Path::new(*d),
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    if let Err(e) = identity_args.resolve("kio-prime", 1) {
        eprintln!("error: {e}");
        return EXIT_USAGE;
    }

    let driver_js = match env::var("KIO_DYN_LOAD_PRIME_DRIVER_JS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            eprintln!(
                "error: KIO_DYN_LOAD_PRIME_DRIVER_JS is not set — the orchestrator must build the \
                 dyn-load-prime driver and export its JS path"
            );
            return EXIT_RUNTIME_FAILURE;
        }
    };

    let execution_mode = match protocol_execution_mode(protocol) {
        Ok(mode) => mode,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    match execute(
        &driver_js,
        image_dir,
        protocol,
        execution_mode,
        protocol.main_module().unwrap_or(""),
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_RUNTIME_FAILURE
        }
    }
}

fn protocol_execution_mode(protocol: RunnerProtocol) -> Result<&'static str, &'static str> {
    if !protocol.supports_dyn_load_prime() {
        return Err("the selected protocol has no complete dyn-load-prime execution adapter");
    }
    match protocol.contract().execution {
        ProtocolExecution::CompileOnly => Ok("load"),
        ProtocolExecution::ConstructOnly => Ok("construct"),
        ProtocolExecution::Invoke(driver) => export_driver_execution_mode(driver),
    }
}

fn export_driver_execution_mode(driver: ExportDriver) -> Result<&'static str, &'static str> {
    match driver {
        ExportDriver::Main { .. } => Ok("main"),
        ExportDriver::NamespaceRoundtrip
        | ExportDriver::CallbackRoundtrip
        | ExportDriver::ModuleRoundtrip
        | ExportDriver::MultilabelRoundtrip
        | ExportDriver::PolyRoundtrip
        | ExportDriver::TypeRoundtrip
        | ExportDriver::CurriedFacade
        | ExportDriver::NewtypeSumRoundtrip
        | ExportDriver::NewtypeScalarRoundtrip
        | ExportDriver::NestedProductRoundtrip
        | ExportDriver::CompoundInputOnce => Ok("script"),
        ExportDriver::PolyCallbackRoundtrip
        | ExportDriver::StructuralRoundtrip
        | ExportDriver::ScalarRoundtrip
        | ExportDriver::HostOwnedRoundtrip
        | ExportDriver::CallableSlotsRoundtrip
        | ExportDriver::RustCallbackAliases
        | ExportDriver::FunctorDictRoundtrip
        | ExportDriver::HostExistentialRoundtrip
        | ExportDriver::PositionalProductRoundtrip
        | ExportDriver::NewtypeVisibilityFacade
        | ExportDriver::NewtypeIgnoredArgumentRoundtrip
        | ExportDriver::RecursiveNewtypeBoundary
        | ExportDriver::NestedCurriedRoundtrip
        | ExportDriver::HostSubstitutedUnitCallback
        | ExportDriver::ReturnedForallCallByValue
        | ExportDriver::FacadeSelectorCollisions
        | ExportDriver::PublicWordNames
        | ExportDriver::ModuleAliasScopeCollision
        | ExportDriver::WideCallable => {
            Err("the selected export protocol has no dyn-load-prime driver script")
        }
        ExportDriver::Coexist => Err(
            "`coexist` requires two host artifacts and is not supported by the one-image dyn-load-prime runner",
        ),
    }
}

fn execute(
    driver_js: &Path,
    image_dir: &Path,
    protocol: RunnerProtocol,
    execution_mode: &str,
    entry_module: &str,
) -> Result<i32, String> {
    if !image_dir.is_dir() {
        return Err(format!("not a directory: {}", image_dir.display()));
    }
    let guest_image = read_guest_image(image_dir)?;
    // KIO_DEBUG_DYN_LOAD_PRIME_IMAGE dumps the assembled guest image (all
    // units in the complete emitted package, concatenated) to stderr — a
    // diagnostic for loader development, not part of the runner's contract.
    if env::var("KIO_DEBUG_DYN_LOAD_PRIME_IMAGE").is_ok() {
        eprintln!("--- guest image ---\n{guest_image}\n--- end guest image ---");
    }
    let driver_src = fs::read_to_string(driver_js)
        .map_err(|e| format!("cannot read driver JS {}: {e}", driver_js.display()))?;
    let driver_display = driver_js.display().to_string();
    // The build-driver contract returns the canonical JS artifact path, whose
    // file stem is the artifact's effective JS namespace. Derive the branded
    // factory from that namespace just as the ordinary JS runner does; do not
    // inspect emitted source or hard-code a generated export spelling.
    let factory_name = driver_factory_name(driver_js)?;

    let runtime = Runtime::new().map_err(|e| format!("creating rquickjs runtime: {e}"))?;
    let context = Context::full(&runtime).map_err(|e| format!("creating rquickjs context: {e}"))?;

    context.with(|ctx| -> Result<i32, String> {
        install_native_callables(&ctx)?;

        let module = Module::declare(
            ctx.clone(),
            "kio-dyn-load-prime-driver",
            driver_src.as_bytes(),
        )
        .catch(&ctx)
        .map_err(|e| format!("parsing {driver_display}: {}", format_caught(&e)))?;
        let (evaluated, eval_promise) = module
            .eval()
            .catch(&ctx)
            .map_err(|e| format!("evaluating {driver_display}: {}", format_caught(&e)))?;
        eval_promise.finish::<()>().catch(&ctx).map_err(|e| {
            format!(
                "draining module evaluation for {driver_display}: {}",
                format_caught(&e)
            )
        })?;

        let create_fn: Function = evaluated
            .get(&*factory_name)
            .catch(&ctx)
            .map_err(|e| format!("reading `{factory_name}` export: {}", format_caught(&e)))?;

        let host_record: Object = ctx
            .eval(
                build_host_record_expression(&guest_image, protocol, execution_mode, entry_module)
                    .into_bytes(),
            )
            .catch(&ctx)
            .map_err(|e| format!("building host record: {}", format_caught(&e)))?;
        let pkg_value: Value = create_fn
            .call((host_record,))
            .catch(&ctx)
            .map_err(|e| format!("calling {factory_name}(host): {}", format_caught(&e)))?;

        let pkg_obj = pkg_value
            .into_object()
            .ok_or_else(|| format!("{factory_name} did not return an object"))?;
        let main_fn = find_driver_main(&pkg_obj)?;
        main_fn
            .call::<_, Value>(())
            .catch(&ctx)
            .map_err(|e| format!("calling driver main(): {}", format_caught(&e)))?;
        Ok(0)
    })
}

/// The driver's branded factory export, derived from its effective JS
/// namespace. `build-driver.sh` returns the canonical `<namespace>.js` path.
fn driver_factory_name(driver_js: &Path) -> Result<String, String> {
    let namespace = driver_js
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("cannot read driver namespace from {}", driver_js.display()))?;
    Ok(format!("create{}", pascal_case(namespace)))
}

/// Read the guest image from a `kio build kio-prime` output directory: the
/// whole emitted tree — every module, host modules and the `.pkg.kio`
/// manifest included — concatenated in path order. This is
/// `dyn_load_prime`'s `load_package` input contract: the loader scans
/// the concatenation (each `module` / `package` header delimits a unit),
/// topo-sorts the modules itself, reads host-fn declarations and the
/// manifest for host awareness, and resolves cross-module references with
/// open-world per-module scoping — so the runner passes the emit through
/// verbatim rather than re-deriving a module subset.
fn read_guest_image(image_dir: &Path) -> Result<String, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    collect_kio_modules(image_dir, &mut candidates)?;
    candidates.sort();
    if candidates.is_empty() {
        return Err(format!(
            "no .kio files under {} — expected a `kio build kio-prime` output tree",
            image_dir.display()
        ));
    }

    let mut texts: Vec<String> = Vec::new();
    for path in &candidates {
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        texts.push(text);
    }
    Ok(texts.join("\n"))
}

/// Collect every `.kio` file (recursively) under an image directory —
/// `.pkg.kio` manifests included, since `load_package` reads the
/// manifest as part of the image.
fn collect_kio_modules(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot iterate {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_kio_modules(&path, out)?;
            continue;
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.ends_with(".kio") {
            out.push(path);
        }
    }
    Ok(())
}

/// Resolve the driver's fixed module-qualified entry.
///
/// The driver package contract exports `main` through its `driver` module
/// namespace. Accepting a flat fallback would hide a facade or namespace
/// regression in the very artifact this differential depends on.
fn find_driver_main<'js>(pkg: &Object<'js>) -> Result<Function<'js>, String> {
    let driver_ns: Value<'js> = pkg
        .get("driver")
        .map_err(|e| format!("driver package exposes no `driver` module: {e}"))?;
    let driver_obj = driver_ns
        .into_object()
        .ok_or_else(|| "driver package `driver` export is not an object".to_owned())?;
    let main_val: Value<'js> = driver_obj
        .get("main")
        .map_err(|e| format!("driver package `driver` module exposes no `main`: {e}"))?;
    main_val
        .as_function()
        .cloned()
        .ok_or_else(|| "driver package `driver.main` is not callable".to_owned())
}

fn role_fixture_key(fixture: RoleFixture) -> &'static str {
    match fixture {
        RoleFixture::Bool => "Bool",
        RoleFixture::I8 => "I8",
        RoleFixture::I16 => "I16",
        RoleFixture::I32 => "I32",
        RoleFixture::I64 => "I64",
        RoleFixture::I128 => "I128",
        RoleFixture::U8 => "U8",
        RoleFixture::U16 => "U16",
        RoleFixture::U32 => "U32",
        RoleFixture::U64 => "U64",
        RoleFixture::U128 => "U128",
        RoleFixture::F32 => "F32",
        RoleFixture::F64 => "F64",
        RoleFixture::String => "String",
    }
}

/// Render the selected protocol's exact host-type projection as a JS value.
///
/// The role and the native literal fixture are deliberately separate fields.
/// A Kio role admits literal syntax; it does not choose a host representation.
fn protocol_host_types_expression(protocol: RunnerProtocol) -> String {
    let entries = protocol
        .contract()
        .host_types
        .iter()
        .map(|binding| {
            let (role, fixture) = match binding.fixture {
                HostTypeFixture::Role(fixture) | HostTypeFixture::SelectedRole(fixture) => {
                    (fixture.role(), role_fixture_key(fixture))
                }
                HostTypeFixture::Array
                | HostTypeFixture::Box
                | HostTypeFixture::Token
                | HostTypeFixture::Scalar => ("", ""),
            };
            format!(
                "{{module:{},leaf:{},role:{},arity:{},fixture:{}}}",
                js_string_literal(binding.module),
                js_string_literal(binding.leaf),
                js_string_literal(role),
                binding.type_arity,
                js_string_literal(fixture),
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(","))
}

/// Render the exact descriptor and application-stage projection for the
/// selected protocol. Both values come from `HostFnBodyKind`'s one structured
/// signature algebra; there is no second free-form descriptor inventory.
fn protocol_host_functions_expression(protocol: RunnerProtocol) -> String {
    let entries = protocol
        .contract()
        .host_fns
        .iter()
        .map(|binding| {
            let groups = binding
                .body
                .canonical_prime_group_slots()
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{module:{},leaf:{},signature:{},groups:[{}]}}",
                js_string_literal(binding.module),
                js_string_literal(binding.leaf),
                js_string_literal(&binding.body.canonical_prime_signature()),
                groups,
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(","))
}

/// Build the JS expression producing the driver's host record. The
/// driver package's emitted JS looks each host item up at
/// `__host__.<module-key>.<member-key>`, so the record nests each fn under its
/// declaring module's JS namespace — the `dyn_load_prime` package's `testapi/*`
/// capabilities plus the driver's own `read_guest_image`. The guest image
/// text is injected as the `read_guest_image` body; every other body is
/// the canonical shape the js / rust runners use, so a guest's host
/// effects evaluate identically to compile-and-run.
///
/// `testapi_scalar` realizes `dyn_load_prime`'s **opaque host scalar** boundary
/// (`test-data/poc/dyn_load_prime/workdir/testapi/scalar.kio`): a guest's host scalars ride
/// through the interpreter as the opaque `Scalar` this host owns, here a
/// tagged `{k, v}` object. `make_scalar(text, representation)` constructs one
/// from a literal's raw text and a fixture-private representation key. The
/// driver's literal callback resolves the loaded `Host_type`'s exact identity
/// against the selected protocol projection and obtains its
/// `HostTypeFixture`; neither its role nor its final path segment selects the
/// representation. The interpreter never parses the scalar or chooses its
/// representation.
fn build_host_record_expression(
    guest_image: &str,
    protocol: RunnerProtocol,
    execution_mode: &str,
    entry_module: &str,
) -> String {
    let img = js_string_literal(guest_image);
    let proto = js_string_literal(protocol.name());
    let mode = js_string_literal(execution_mode);
    let entry = js_string_literal(entry_module);
    let protocol_types = protocol_host_types_expression(protocol);
    let protocol_functions = protocol_host_functions_expression(protocol);
    format!(
        r#"(() => {{
  const protocolHostTypes = {protocol_types};
  const protocolHostFunctions = {protocol_functions};
  return {{
  driver: {{
    readGuestImage: () => {img},
    protocolName: () => {proto},
    protocolExecutionMode: () => {mode},
    protocolEntryModule: () => {entry},
    runnerFailure: (message) => {{ throw new Error(message); }},
    protocolEprint: (message) => __kio_host_eprint__(message),
    protocolExit: (status) => __kio_host_exit__(status),
    protocolHostTypeCount: () => protocolHostTypes.length,
    protocolHostTypeModule: (index) => protocolHostTypes[index].module,
    protocolHostTypeLeaf: (index) => protocolHostTypes[index].leaf,
    protocolHostTypeRole: (index) => protocolHostTypes[index].role,
    protocolHostTypeArity: (index) => protocolHostTypes[index].arity,
    protocolLiteralRepresentation: (module, leaf) => {{
      const matches = protocolHostTypes.filter((entry) => entry.module === module && entry.leaf === leaf);
      if (matches.length !== 1 || matches[0].fixture === '') throw new Error('protocol literal type is not one exact role-bearing host type: ' + module + '.' + leaf);
      return matches[0].fixture;
    }},
    protocolHostFnCount: () => protocolHostFunctions.length,
    protocolHostFnModule: (index) => protocolHostFunctions[index].module,
    protocolHostFnLeaf: (index) => protocolHostFunctions[index].leaf,
    protocolHostFnSignature: (index) => protocolHostFunctions[index].signature,
    protocolHostFnGroupCount: (index) => protocolHostFunctions[index].groups.length,
    protocolHostFnGroupSlots: (fnIndex, groupIndex) => protocolHostFunctions[fnIndex].groups[groupIndex],
  }},
  testapi_io: {{
    print: (s) => __kio_host_print__(s),
    readAsciiLine: () => {{ const line = __kio_host_read_ascii_line__(); if (line == null) return {{ _1: null }}; return {{ _0: line }}; }},
  }},
  testapi_arith: {{
    add: (a, b) => Number(BigInt.asIntN(32, BigInt(a) + BigInt(b))),
    sub: (a, b) => Number(BigInt.asIntN(32, BigInt(a) - BigInt(b))),
    mul: (a, b) => Number(BigInt.asIntN(32, BigInt(a) * BigInt(b))),
    div: (a, b) => Number(BigInt.asIntN(32, BigInt(a) / BigInt(b))),
    mod: (a, b) => Number(BigInt.asIntN(32, BigInt(a) % BigInt(b))),
    addI32: (a, b) => Number(BigInt.asIntN(32, BigInt(a) + BigInt(b))),
    subI32: (a, b) => Number(BigInt.asIntN(32, BigInt(a) - BigInt(b))),
    mulI32: (a, b) => Number(BigInt.asIntN(32, BigInt(a) * BigInt(b))),
    divI32: (a, b) => Number(BigInt.asIntN(32, BigInt(a) / BigInt(b))),
    modI32: (a, b) => Number(BigInt.asIntN(32, BigInt(a) % BigInt(b))),
    leqI32: (a, b) => a <= b,
    ltI32: (a, b) => a < b,
    eqI32: (a, b) => a === b,
    addF64: (a, b) => a + b,
    subF64: (a, b) => a - b,
    mulF64: (a, b) => a * b,
    divF64: (a, b) => a / b,
  }},
  testapi_fmt: {{
    intToString: (n) => String(n),
    boolToString: (v) => String(v),
    printI32: (n) => __kio_host_print__(String(n)),
    i32ToString: (n) => String(n),
    f64ToString: (n) => String(n),
    stringToInt: (s) => {{ if (!/^[+-]?\d+$/.test(s)) return {{ _1: null }}; const n = BigInt(s); if (n < -2147483648n || n > 2147483647n) return {{ _1: null }}; return {{ _0: Number(n) }}; }},
  }},
  testapi_text: {{
    stringConcat: (a, b) => a + b,
    stringEq: (a, b) => a === b,
    stringLen: (s) => s.length,
    stringSlice: (s, start, end) => {{ if (start < 0 || start > end || end > s.length) throw new Error('string_slice: invalid range [' + start + ', ' + end + ') for len ' + s.length); return s.slice(start, end); }},
    stringCodeAt: (s, index) => {{ if (index < 0 || index >= s.length) return {{ _1: null }}; return {{ _0: s.charCodeAt(index) }}; }},
  }},
  testapi_iter: {{
    loop: (step, s) => {{ while (true) {{ const out = step(s); if (out !== null && typeof out === 'object') {{ if (Object.prototype.hasOwnProperty.call(out, '_0')) {{ s = out._0; continue; }} if (Object.prototype.hasOwnProperty.call(out, '_1')) return out._1; }} if (Array.isArray(out)) {{ if (out[0] === 0) {{ s = out[1]; continue; }} return out[1]; }} throw new Error('loop: step returned invalid sum'); }} }},
  }},
  testapi_scalar: {{
    makeScalar: (text, representation) => {{
      if (representation === 'I8') return {{ k: 'i8', v: Number(BigInt.asIntN(8, BigInt(text))) }};
      if (representation === 'I16') return {{ k: 'i16', v: Number(BigInt.asIntN(16, BigInt(text))) }};
      if (representation === 'I32') return {{ k: 'i32', v: Number(BigInt.asIntN(32, BigInt(text))) }};
      if (representation === 'I64') return {{ k: 'i64', v: BigInt.asIntN(64, BigInt(text)) }};
      if (representation === 'I128') return {{ k: 'i128', v: BigInt.asIntN(128, BigInt(text)) }};
      if (representation === 'U8') return {{ k: 'u8', v: Number(BigInt.asUintN(8, BigInt(text))) }};
      if (representation === 'U16') return {{ k: 'u16', v: Number(BigInt.asUintN(16, BigInt(text))) }};
      if (representation === 'U32') return {{ k: 'u32', v: Number(BigInt.asUintN(32, BigInt(text))) }};
      if (representation === 'U64') return {{ k: 'u64', v: BigInt.asUintN(64, BigInt(text)) }};
      if (representation === 'U128') return {{ k: 'u128', v: BigInt.asUintN(128, BigInt(text)) }};
      if (representation === 'F32') return {{ k: 'f32', v: Math.fround(Number(text)) }};
      if (representation === 'F64') return {{ k: 'f64', v: Number(text) }};
      if (representation === 'String') return {{ k: 'str', v: text }};
      if (representation === 'Bool') return {{ k: 'bool', v: text === 't' }};
      throw new Error('make_scalar: unknown representation key ' + representation);
    }},
    scalarOfI32: (n) => ({{ k: 'i32', v: n }}),
    scalarOfStr: (s) => ({{ k: 'str', v: s }}),
    scalarOfBool: (b) => ({{ k: 'bool', v: b }}),
    scalarOfF64: (n) => ({{ k: 'f64', v: n }}),
    scalarAsI32: (s) => (s.k === 'i32' ? {{ _1: s.v }} : {{ _0: null }}),
    scalarAsStr: (s) => (s.k === 'str' ? {{ _1: s.v }} : {{ _0: null }}),
    scalarAsBool: (s) => (s.k === 'bool' ? {{ _1: s.v }} : {{ _0: null }}),
    scalarAsF64: (s) => (s.k === 'f64' ? {{ _1: s.v }} : {{ _0: null }}),
    scalarIsTrue: (s) => s.v === true,
  }},
  }};
}})()"#
    )
}

/// Register the I/O / process callables on the global object. The host
/// record's arrow functions reach these by name, matching the JS runner's
/// `install_native_callables` so `print` writes to this process's stdout.
fn install_native_callables(ctx: &Ctx<'_>) -> Result<(), String> {
    let globals = ctx.globals();
    globals
        .set("__kio_host_print__", Func::from(host_print))
        .map_err(|e| format!("registering print: {e}"))?;
    globals
        .set("__kio_host_eprint__", Func::from(host_eprint))
        .map_err(|e| format!("registering eprint: {e}"))?;
    globals
        .set("__kio_host_exit__", Func::from(host_exit))
        .map_err(|e| format!("registering exit: {e}"))?;
    globals
        .set(
            "__kio_host_read_ascii_line__",
            Func::from(host_read_ascii_line),
        )
        .map_err(|e| format!("registering read_ascii_line: {e}"))?;
    Ok(())
}

fn host_print(value: Value<'_>) {
    if let Some(s) = value.as_string()
        && let Ok(s) = s.to_string()
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}

fn host_eprint(value: Value<'_>) {
    if let Some(s) = value.as_string()
        && let Ok(s) = s.to_string()
    {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(s.as_bytes());
        let _ = err.flush();
    }
}

fn host_exit(status: i32) {
    process::exit(status);
}

fn host_read_ascii_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            if !line.is_ascii() {
                eprintln!("read_ascii_line: non-ASCII input");
                process::exit(1);
            }
            Some(line)
        }
        Err(e) => {
            eprintln!("read_ascii_line: reading stdin: {e}");
            process::exit(1);
        }
    }
}

fn format_caught(err: &rquickjs::CaughtError<'_>) -> String {
    err.to_string()
}

fn js_string_literal(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(label: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "kio-test-runner-dyn-load-prime-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&p).expect("create scratch dir");
            ScratchDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_guest_image_errors_on_empty_dir() {
        let dir = ScratchDir::new("empty");
        let err = read_guest_image(dir.path()).expect_err("expected error");
        assert!(err.contains("no .kio files"), "unexpected: {err}");
    }

    #[test]
    fn driver_factory_follows_the_artifact_namespace() {
        assert_eq!(
            driver_factory_name(Path::new("/build/out/js/dyn_load_prime_driver.js")).as_deref(),
            Ok("createDynLoadPrimeDriver")
        );
    }

    #[test]
    fn driver_main_requires_the_exact_module_namespace() {
        let runtime = Runtime::new().expect("runtime");
        let context = Context::full(&runtime).expect("context");
        context.with(|ctx| {
            let exact: Object<'_> = ctx
                .eval("({driver:{main:()=>null}})")
                .expect("exact driver package");
            assert!(find_driver_main(&exact).is_ok());

            let flat: Object<'_> = ctx.eval("({main:()=>null})").expect("flat package");
            let error = find_driver_main(&flat).expect_err("flat main must not be accepted");
            assert!(error.contains("`driver`"), "unexpected: {error}");
        });
    }

    #[test]
    fn read_guest_image_concatenates_the_whole_tree() {
        let dir = ScratchDir::new("whole-tree");
        fs::write(
            dir.path().join("nums.kio"),
            "module nums;\n\npub fn one() -> . { () }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("main.kio"),
            "module main;\n\nuse one from nums;\n\npub fn main() -> . { one() }\n",
        )
        .unwrap();
        // A module the entry does not import (a vendored compile-time-only
        // library), a pure-host module, and the manifest are all part of
        // `load_package`'s input and ride through verbatim.
        fs::write(
            dir.path().join("elab.kio"),
            "module elab;\n\npub fn dead() -> . { () }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("hostapi.kio"),
            "module hostapi;\n\nhost fn print(p0: String) -> .;\n",
        )
        .unwrap();
        fs::write(dir.path().join("g.pkg.kio"), "package g;\n").unwrap();
        let img = read_guest_image(dir.path()).expect("read tree");
        assert!(img.contains("module nums;"), "nums included: {img}");
        assert!(img.contains("module main;"), "main included: {img}");
        assert!(img.contains("module elab;"), "elab included: {img}");
        assert!(img.contains("module hostapi;"), "host included: {img}");
        assert!(img.contains("package g;"), "manifest included: {img}");
    }

    #[test]
    fn host_record_injects_image_and_canonical_bodies() {
        let s = build_host_record_expression(
            "module g;\npub fn main() -> . { () }\n",
            RunnerProtocol::TestApiIo,
            "main",
            "testapi/main",
        );
        assert!(s.contains("readGuestImage: () => \"module g;"));
        assert!(s.contains("protocolName: () => \"testapi-io\""));
        assert!(s.contains("protocolExecutionMode: () => \"main\""));
        assert!(s.contains("protocolEntryModule: () => \"testapi/main\""));
        assert!(s.contains("protocolEprint: (message) => __kio_host_eprint__(message)"));
        assert!(s.contains("protocolExit: (status) => __kio_host_exit__(status)"));
        assert!(s.contains("protocolLiteralRepresentation: (module, leaf)"));
        assert!(s.contains(
            "module:\"testapi/io\",leaf:\"eprint\",signature:\"(h{testapi.String}) -> .\",groups:[1]"
        ));
        assert!(s.contains(
            "module:\"testapi/io\",leaf:\"exit\",signature:\"(h{testapi.I32}) -> !\",groups:[1]"
        ));
        assert!(s.contains("print: (s) => __kio_host_print__(s)"));
        assert!(s.contains("addI32: (a, b) => Number(BigInt.asIntN(32"));
        assert!(s.contains("add: (a, b) => Number(BigInt.asIntN(32"));
        assert!(s.contains("mod: (a, b) => Number(BigInt.asIntN(32"));
        assert!(s.contains("boolToString: (v) => String(v)"));
        assert!(s.contains("printI32: (n) => __kio_host_print__(String(n))"));
        assert!(s.contains("stringConcat: (a, b) => a + b"));
        assert!(!s.contains("__kio_prime_eval_pkg__"));
    }

    #[test]
    fn driver_host_uses_exact_public_module_and_member_names() {
        let expression = build_host_record_expression(
            "module guest_source;",
            RunnerProtocol::TestApiIo,
            "main",
            "testapi/main",
        );
        let expected: &[(&str, &[&str])] = &[
            (
                "driver",
                &[
                    "readGuestImage",
                    "protocolName",
                    "protocolExecutionMode",
                    "protocolEntryModule",
                    "runnerFailure",
                    "protocolEprint",
                    "protocolExit",
                    "protocolHostTypeCount",
                    "protocolHostTypeModule",
                    "protocolHostTypeLeaf",
                    "protocolHostTypeRole",
                    "protocolHostTypeArity",
                    "protocolLiteralRepresentation",
                    "protocolHostFnCount",
                    "protocolHostFnModule",
                    "protocolHostFnLeaf",
                    "protocolHostFnSignature",
                    "protocolHostFnGroupCount",
                    "protocolHostFnGroupSlots",
                ],
            ),
            ("testapi_io", &["print", "readAsciiLine"]),
            (
                "testapi_arith",
                &[
                    "add", "sub", "mul", "div", "mod", "addI32", "subI32", "mulI32", "divI32",
                    "modI32", "leqI32", "ltI32", "eqI32", "addF64", "subF64", "mulF64", "divF64",
                ],
            ),
            (
                "testapi_fmt",
                &[
                    "intToString",
                    "boolToString",
                    "printI32",
                    "i32ToString",
                    "f64ToString",
                    "stringToInt",
                ],
            ),
            (
                "testapi_text",
                &[
                    "stringConcat",
                    "stringEq",
                    "stringLen",
                    "stringSlice",
                    "stringCodeAt",
                ],
            ),
            ("testapi_iter", &["loop"]),
            (
                "testapi_scalar",
                &[
                    "makeScalar",
                    "scalarOfI32",
                    "scalarOfStr",
                    "scalarOfBool",
                    "scalarOfF64",
                    "scalarAsI32",
                    "scalarAsStr",
                    "scalarAsBool",
                    "scalarAsF64",
                    "scalarIsTrue",
                ],
            ),
        ];
        let runtime = Runtime::new().expect("runtime");
        let context = Context::full(&runtime).expect("context");
        context.with(|ctx| {
            let host: Object<'_> = ctx.eval(expression).expect("host record");
            let mut actual_modules = host
                .keys::<String>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let mut expected_modules = expected
                .iter()
                .map(|(module, _)| *module)
                .collect::<Vec<_>>();
            actual_modules.sort();
            expected_modules.sort();
            assert_eq!(actual_modules, expected_modules);
            for (module, members) in expected {
                let module_object: Object<'_> = host.get(*module).expect("public host module");
                let mut actual = module_object
                    .keys::<String>()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let mut expected_members = members.to_vec();
                actual.sort();
                expected_members.sort();
                assert_eq!(actual, expected_members, "{module}");
                for member in *members {
                    let _: Function<'_> =
                        module_object.get(*member).expect("callable public member");
                }
            }
            let driver: Object<'_> = host.get("driver").unwrap();
            let image: Function<'_> = driver.get("readGuestImage").unwrap();
            assert_eq!(image.call::<_, String>(()).unwrap(), "module guest_source;");
            let entry: Function<'_> = driver.get("protocolEntryModule").unwrap();
            assert_eq!(entry.call::<_, String>(()).unwrap(), "testapi/main");
        });
    }

    #[test]
    fn literal_fixture_projection_uses_exact_identity_not_role_or_leaf() {
        let types = protocol_host_types_expression(RunnerProtocol::SameLeafHostLiteralRoles);
        assert!(
            types
                .contains("{module:\"left\",leaf:\"Shared\",role:\"i64\",arity:0,fixture:\"I64\"}"),
            "left.Shared projection: {types}"
        );
        assert!(
            types.contains(
                "{module:\"right\",leaf:\"Shared\",role:\"i32\",arity:0,fixture:\"I32\"}"
            ),
            "right.Shared projection: {types}"
        );
        assert!(
            !types.contains("fixture:\"String\""),
            "parallel table leaked: {types}"
        );
    }

    #[test]
    fn canonical_prime_projection_covers_polymorphic_and_structural_shapes() {
        let host_box = protocol_host_functions_expression(RunnerProtocol::HostGenericTypeRoundtrip);
        assert!(
            host_box.contains(
                "leaf:\"box_get\",signature:\"[#0] (h{testapi.Box}(#0)) -> #0\",groups:[1]"
            ),
            "box projection: {host_box}"
        );

        let callback = protocol_host_functions_expression(RunnerProtocol::HostCallbackRoundtrip);
        assert!(
            callback.contains(
                "leaf:\"call_step\",signature:\"(((h{testapi.I32} & (h{testapi.String} & h{testapi.Bool})) -> h{testapi.I32}) & h{testapi.I32}) -> h{testapi.I32}\",groups:[2]"
            ),
            "callback projection: {callback}"
        );

        let staged =
            protocol_host_functions_expression(RunnerProtocol::HostInterleavedStageRoundtrip);
        assert!(
            staged.contains(
                "leaf:\"staged\",signature:\"[#0] (h{testapi.String}) -> [#1] (h{testapi.String}) -> h{testapi.String}\",groups:[1,1]"
            ),
            "interleaved-stage projection: {staged}"
        );
    }

    #[test]
    fn execution_mode_follows_the_complete_shared_protocol_contract() {
        assert_eq!(
            protocol_execution_mode(RunnerProtocol::CompileOnly),
            Ok("load")
        );
        assert_eq!(
            protocol_execution_mode(RunnerProtocol::ConstructOnly),
            Ok("construct")
        );
        assert_eq!(protocol_execution_mode(RunnerProtocol::Empty), Ok("main"));
        assert_eq!(
            protocol_execution_mode(RunnerProtocol::TestApiPrint),
            Ok("main")
        );
        assert_eq!(
            protocol_execution_mode(RunnerProtocol::ExportNamespaceRoundtrip),
            Ok("script")
        );
        assert!(protocol_execution_mode(RunnerProtocol::Coexist).is_err());
        assert!(protocol_execution_mode(RunnerProtocol::TestApiArray).is_err());
        assert!(protocol_execution_mode(RunnerProtocol::TestApiBigint).is_err());
        assert!(protocol_execution_mode(RunnerProtocol::TestApiFloatF32F64).is_err());
    }

    #[test]
    fn protocol_support_classification_matches_execution_dispatch() {
        for &protocol in RunnerProtocol::ALL {
            assert_eq!(
                protocol.supports_dyn_load_prime(),
                protocol_execution_mode(protocol).is_ok(),
                "dyn-load-prime support drift for `{}`",
                protocol.name()
            );
        }
    }

    #[test]
    fn unknown_protocol_is_a_usage_error() {
        assert_eq!(
            run(&["--protocol".to_owned(), "not-a-runner-protocol".to_owned(),]),
            EXIT_USAGE
        );
    }

    #[test]
    fn every_export_driver_has_the_dyn_execution_class_it_requires() {
        for driver in [
            ExportDriver::Main { module: "main" },
            ExportDriver::Main {
                module: "testapi/main",
            },
        ] {
            assert_eq!(export_driver_execution_mode(driver), Ok("main"));
        }
        for driver in [
            ExportDriver::NamespaceRoundtrip,
            ExportDriver::CallbackRoundtrip,
            ExportDriver::ModuleRoundtrip,
            ExportDriver::MultilabelRoundtrip,
            ExportDriver::PolyRoundtrip,
            ExportDriver::TypeRoundtrip,
            ExportDriver::CurriedFacade,
            ExportDriver::NewtypeSumRoundtrip,
            ExportDriver::NewtypeScalarRoundtrip,
            ExportDriver::NestedProductRoundtrip,
            ExportDriver::CompoundInputOnce,
        ] {
            assert_eq!(export_driver_execution_mode(driver), Ok("script"));
        }
        for driver in [
            ExportDriver::PolyCallbackRoundtrip,
            ExportDriver::StructuralRoundtrip,
            ExportDriver::ScalarRoundtrip,
            ExportDriver::HostOwnedRoundtrip,
            ExportDriver::CallableSlotsRoundtrip,
            ExportDriver::RustCallbackAliases,
            ExportDriver::FunctorDictRoundtrip,
            ExportDriver::HostExistentialRoundtrip,
            ExportDriver::PositionalProductRoundtrip,
            ExportDriver::WideCallable,
            ExportDriver::NewtypeVisibilityFacade,
            ExportDriver::NewtypeIgnoredArgumentRoundtrip,
            ExportDriver::RecursiveNewtypeBoundary,
            ExportDriver::NestedCurriedRoundtrip,
            ExportDriver::HostSubstitutedUnitCallback,
            ExportDriver::ReturnedForallCallByValue,
            ExportDriver::FacadeSelectorCollisions,
            ExportDriver::PublicWordNames,
            ExportDriver::ModuleAliasScopeCollision,
            ExportDriver::Coexist,
        ] {
            assert!(export_driver_execution_mode(driver).is_err());
        }
    }
}

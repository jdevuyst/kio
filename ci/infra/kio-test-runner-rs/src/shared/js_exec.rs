//! Shared JS execution engine for the `kio build js` ES module — used
//! by both `kio-test-runner-js` and `kio-test-runner-ts`.
//!
//! `kio build ts` writes the JS backend's `<pkg>.js` byte-identical
//! (`specs/backends/ts.md` § Output layout) plus a `<pkg>.d.ts` skin the
//! runner ignores (the runner runs the `.js`). So the TS runner runs
//! exactly the same artifact, the same way, as the JS runner: this
//! module holds that engine once — the host record builders, native
//! callables, protocol drivers, and exact `<ns>.js` loading — and each
//! bin is a thin `fn main` that parses argv, picks the protocol, and
//! drives a [`JsRunner`].
//!
//! The engine evaluates the package module via rquickjs (Rust bindings
//! around the QuickJS C engine). The harness supplies the artifact
//! namespace independently of emitted source; the runner uses it to
//! address `<ns>.js` and derive the branded `create<Handle>` factory per
//! `specs/backends/js.md` § Output layout. It builds the host record from
//! the selected **protocol** alone — installing each binding's structured
//! body at its exact module+leaf identity — and reads no `.kio` source or generated
//! interface to reconstruct either contract. For the full canonical-body
//! catalogue and protocol model, see the `kio-test-runner-js` bin's
//! module docs and `ci/infra/kio-test-runner-rs/README.md`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

use rquickjs::function::Func;
use rquickjs::{CatchResultExt, Context, Ctx, Function, Module, Object, Runtime, Value};

// The `host_api` / `protocol` / `runner` modules are
// declared at the **crate root** by each bin (their cross-references use
// `crate::<mod>` paths), so the engine reaches them through `crate::`
// rather than re-declaring its own includes (which would double-compile
// them in one bin crate, and break those `crate::`-rooted
// cross-references). Each bin includes this engine plus those shared
// shared modules; both run the same `<pkg>.js`.
use crate::artifact_identity::{ArtifactIdentity, pascal_case};
use crate::host_api::{self, HostApi};
use crate::protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, ProtocolExecution, RunnerProtocol,
    WIDE_CALLABLE_SLOT_COUNT,
};
use crate::runner::TestRunner;

const FLOAT_KINDS: &[&str] = &["f32", "f64"];

fn require_package_object<'js>(
    value: Value<'js>,
    factory_name: &str,
) -> Result<Object<'js>, String> {
    value
        .into_object()
        .ok_or_else(|| format!("{factory_name}(host) returned a non-object"))
}

fn integer_bit_width(kind: &str) -> Option<u32> {
    match kind {
        "i8" | "u8" => Some(8),
        "i16" | "u16" => Some(16),
        "i32" | "u32" => Some(32),
        "i64" | "u64" => Some(64),
        "i128" | "u128" => Some(128),
        _ => None,
    }
}

fn is_signed_integer(kind: &str) -> bool {
    matches!(kind, "i8" | "i16" | "i32" | "i64" | "i128")
}

fn is_unsigned_integer(kind: &str) -> bool {
    matches!(kind, "u8" | "u16" | "u32" | "u64" | "u128")
}

/// The JS execution engine's `TestRunner` implementation, shared by the
/// `kio-test-runner-js` and `kio-test-runner-ts` bins (the `ts` backend's
/// `<pkg>.js` is the JS backend's, byte-identical, so both run it the
/// same way).
///
/// A fresh rquickjs `Runtime`/`Context` is built per case. The
/// protocol is carried here so `host_api` — whose trait
/// signature takes no protocol — can build the host record from the
/// protocol's fixed env.
pub struct JsRunner {
    protocol: RunnerProtocol,
    identity: ArtifactIdentity,
}

impl JsRunner {
    /// Build the engine for a `protocol`. Each bin's `fn main`
    /// constructs one after parsing argv and drives it via
    /// [`TestRunner::run`].
    pub fn new(protocol: RunnerProtocol, identity: ArtifactIdentity) -> Self {
        Self { protocol, identity }
    }
}

impl TestRunner for JsRunner {
    fn host_api(&self) -> HostApi {
        host_api::dynamic_host_api(self.protocol.contract())
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        // Address the exact module named by the harness-supplied artifact
        // identity. A missing file means the artifact was not built (or
        // the build violated its published output contract).
        let package_module_file = package_js_module(output_dir, &self.identity.namespace)?;
        let factory_name = js_factory_name(&self.identity.namespace);
        let src = fs::read_to_string(&package_module_file)
            .map_err(|e| format!("cannot read {}: {e}", package_module_file.display()))?;
        let package_module_display = package_module_file.display().to_string();
        let runtime = Runtime::new().map_err(|e| format!("creating rquickjs runtime: {e}"))?;
        let context =
            Context::full(&runtime).map_err(|e| format!("creating rquickjs context: {e}"))?;

        context.with(|ctx| -> Result<i32, String> {
            install_native_callables(&ctx)?;

            // Declare + evaluate the package module. `Module::declare`
            // parses and stages the module; `eval()` then runs it and
            // hands back the evaluated module plus a promise that
            // settles once module evaluation completes. We drive the
            // promise to completion synchronously via `finish::<()>`
            // — the package module is fully synchronous so the
            // QuickJS job queue drains in one pass.
            let module = Module::declare(ctx.clone(), "kio-export", src.as_bytes())
                .catch(&ctx)
                .map_err(|e| format!("parsing {package_module_display}: {}", format_caught(&e)))?;
            let (evaluated, eval_promise) = module.eval().catch(&ctx).map_err(|e| {
                format!("evaluating {package_module_display}: {}", format_caught(&e))
            })?;
            eval_promise.finish::<()>().catch(&ctx).map_err(|e| {
                format!(
                    "draining module evaluation for {package_module_display}: {}",
                    format_caught(&e)
                )
            })?;

            if matches!(
                protocol.contract().execution,
                ProtocolExecution::CompileOnly
            ) {
                return Ok(0);
            }

            // Pull the branded `create<Handle>` factory out of the
            // module's evaluated exports.
            let create_fn: Function = evaluated
                .get(&*factory_name)
                .catch(&ctx)
                .map_err(|e| format!("reading `{factory_name}` export: {}", format_caught(&e)))?;

            // Build the runner's default host record by evaluating
            // the record-expression against the prepared context,
            // then call the factory to instantiate the package.
            let host_record: Object = ctx
                .eval(build_host_record_expression(host, protocol).into_bytes())
                .catch(&ctx)
                .map_err(|e| format!("building host record: {}", format_caught(&e)))?;
            let pkg_value: Value = create_fn
                .call((host_record,))
                .catch(&ctx)
                .map_err(|e| format!("calling {factory_name}(host): {}", format_caught(&e)))?;

            let pkg_obj = require_package_object(pkg_value, &factory_name)?;
            run_js_protocol(&ctx, protocol, &pkg_obj)?;
            Ok(0)
        })
    }
}

/// The `coexist` protocol's two-artifact execution (`protocol.rs`
/// § The coexist protocol): one QuickJS context evaluates both emitted
/// modules, instantiates each against a host whose `print` prefixes
/// the package's namespace, and interleaves the calls first → second →
/// first. Shared by the js and ts bins (the ts artifact's `.js` is the
/// JS backend's byte-identical).
pub fn run_coexist(
    dir_a: &Path,
    identity_a: &ArtifactIdentity,
    dir_b: &Path,
    identity_b: &ArtifactIdentity,
) -> Result<i32, String> {
    let file_a = package_js_module(dir_a, &identity_a.namespace)?;
    let file_b = package_js_module(dir_b, &identity_b.namespace)?;
    let (ns_a, ns_b) = (&identity_a.namespace, &identity_b.namespace);
    if ns_a == ns_b {
        return Err(format!(
            "coexist requires two distinct package namespaces; both artifacts are `{ns_a}`"
        ));
    }
    let runtime = Runtime::new().map_err(|e| format!("creating rquickjs runtime: {e}"))?;
    let context = Context::full(&runtime).map_err(|e| format!("creating rquickjs context: {e}"))?;
    context.with(|ctx| -> Result<i32, String> {
        install_native_callables(&ctx)?;
        let instantiate = |file: &Path, ns: &str, prefix: &str| -> Result<Object<'_>, String> {
            let src = fs::read_to_string(file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let module = Module::declare(ctx.clone(), format!("kio-coexist-{ns}"), src.as_bytes())
                .catch(&ctx)
                .map_err(|e| format!("parsing {}: {}", file.display(), format_caught(&e)))?;
            let (evaluated, eval_promise) = module
                .eval()
                .catch(&ctx)
                .map_err(|e| format!("evaluating {}: {}", file.display(), format_caught(&e)))?;
            eval_promise
                .finish::<()>()
                .catch(&ctx)
                .map_err(|e| format!("draining {}: {}", file.display(), format_caught(&e)))?;
            let factory_name = js_factory_name(ns);
            let create_fn: Function = evaluated
                .get(&*factory_name)
                .catch(&ctx)
                .map_err(|e| format!("reading `{factory_name}` export: {}", format_caught(&e)))?;
            let host_record: Object = ctx
                .eval(
                    format!(
                        r#"({{ greeter: {{ print: (s) => __kio_host_print__("{prefix}: " + s) }} }})"#
                    )
                    .into_bytes(),
                )
                .catch(&ctx)
                .map_err(|e| format!("building {ns} host record: {}", format_caught(&e)))?;
            let pkg_value: Value = create_fn
                .call((host_record,))
                .catch(&ctx)
                .map_err(|e| format!("calling {factory_name}(host): {}", format_caught(&e)))?;
            pkg_value
                .into_object()
                .ok_or_else(|| format!("{factory_name}(host) returned a non-object"))
        };
        let pkg_a = instantiate(&file_a, ns_a, "first")?;
        let pkg_b = instantiate(&file_b, ns_b, "second")?;
        let main_selector = host_api::facade_module_selector("main", false);
        let call_main = |pkg: &Object<'_>, ns: &str| -> Result<(), String> {
            let greeter: Object = pkg
                .get("greeter")
                .catch(&ctx)
                .map_err(|e| format!("{ns}: reading pkg.greeter: {}", format_caught(&e)))?;
            let main_ns: Object = greeter
                .get(main_selector.as_str())
                .catch(&ctx)
                .map_err(|e| {
                    format!("{ns}: reading module `greeter/main`: {}", format_caught(&e))
                })?;
            let main_fn: Function = main_ns
                .get("main")
                .catch(&ctx)
                .map_err(|e| {
                    format!(
                        "{ns}: reading export `greeter/main.main`: {}",
                        format_caught(&e)
                    )
                })?;
            main_fn
                .call::<_, Value>(())
                .catch(&ctx)
                .map_err(|e| {
                    format!(
                        "{ns}: calling export `greeter/main.main`: {}",
                        format_caught(&e)
                    )
                })?;
            Ok(())
        };
        // The minted-shape half of the witness: both packages export the
        // same `pair() -> (I32 & String)`, so each artifact mints the
        // structurally identical positional product; reading `_0` / `_1`
        // from both in one program proves the mints coexist.
        let call_pair = |pkg: &Object<'_>, ns: &str, label: &str| -> Result<(), String> {
            let greeter: Object = pkg
                .get("greeter")
                .catch(&ctx)
                .map_err(|e| format!("{ns}: reading pkg.greeter: {}", format_caught(&e)))?;
            let main_ns: Object = greeter
                .get(main_selector.as_str())
                .catch(&ctx)
                .map_err(|e| {
                    format!("{ns}: reading module `greeter/main`: {}", format_caught(&e))
                })?;
            let pair_fn: Function = main_ns
                .get("pair")
                .catch(&ctx)
                .map_err(|e| {
                    format!(
                        "{ns}: reading export `greeter/main.pair`: {}",
                        format_caught(&e)
                    )
                })?;
            let q: Object = pair_fn
                .call(())
                .catch(&ctx)
                .map_err(|e| {
                    format!(
                        "{ns}: calling export `greeter/main.pair`: {}",
                        format_caught(&e)
                    )
                })?;
            let n: i32 = q
                .get("_0")
                .catch(&ctx)
                .map_err(|e| format!("{ns}: reading pair()._0: {}", format_caught(&e)))?;
            let s: String = q
                .get("_1")
                .catch(&ctx)
                .map_err(|e| format!("{ns}: reading pair()._1: {}", format_caught(&e)))?;
            println!("{label} pair: {n} {s}");
            Ok(())
        };
        call_main(&pkg_a, ns_a)?;
        call_main(&pkg_b, ns_b)?;
        call_main(&pkg_a, ns_a)?;
        call_pair(&pkg_a, ns_a, "first")?;
        call_pair(&pkg_b, ns_b, "second")?;
        call_pair(&pkg_a, ns_a, "first")?;
        Ok(0)
    })
}

fn run_js_protocol<'js>(
    ctx: &Ctx<'js>,
    protocol: RunnerProtocol,
    pkg_obj: &Object<'js>,
) -> Result<(), String> {
    match protocol.contract().execution {
        ProtocolExecution::CompileOnly | ProtocolExecution::ConstructOnly => Ok(()),
        ProtocolExecution::Invoke(ExportDriver::Main { module }) => {
            run_default_main_js(ctx, pkg_obj, module)
        }
        ProtocolExecution::Invoke(ExportDriver::NamespaceRoundtrip) => {
            run_export_namespace_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::CallbackRoundtrip) => {
            run_export_callback_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::ModuleRoundtrip) => {
            run_export_module_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::MultilabelRoundtrip) => {
            run_export_multilabel_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::PolyRoundtrip) => {
            run_export_poly_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::PolyCallbackRoundtrip) => {
            run_export_poly_callback_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::StructuralRoundtrip) => {
            run_export_structural_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(
            ExportDriver::ScalarRoundtrip | ExportDriver::HostOwnedRoundtrip,
        ) => run_export_native_payload_roundtrip_js(ctx, pkg_obj, protocol),
        ProtocolExecution::Invoke(ExportDriver::PositionalProductRoundtrip) => {
            run_export_positional_product_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::CallableSlotsRoundtrip) => {
            run_export_callable_slots_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::RustCallbackAliases) => {
            Err("rust-callback-aliases tests the Rust public naming contract only".to_owned())
        }
        ProtocolExecution::Invoke(ExportDriver::FunctorDictRoundtrip) => {
            run_export_functor_dict_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::HostExistentialRoundtrip) => {
            run_host_existential_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::TypeRoundtrip) => {
            run_export_type_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeSumRoundtrip) => {
            run_export_newtype_sum_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeScalarRoundtrip) => {
            run_export_newtype_scalar_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeIgnoredArgumentRoundtrip) => {
            run_export_newtype_ignored_argument_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::RecursiveNewtypeBoundary) => {
            run_recursive_newtype_boundary_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeVisibilityFacade) => {
            run_newtype_visibility_facade_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::NestedProductRoundtrip) => {
            run_export_nested_product_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::CompoundInputOnce) => {
            run_compound_input_once_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::CurriedFacade) => {
            run_export_curried_facade_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::NestedCurriedRoundtrip) => {
            run_nested_curried_roundtrip_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::HostSubstitutedUnitCallback) => {
            run_host_substituted_unit_callback_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::ReturnedForallCallByValue) => {
            run_returned_forall_call_by_value_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::FacadeSelectorCollisions) => {
            run_facade_selector_collisions_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::PublicWordNames) => {
            run_public_word_names_js(ctx, pkg_obj)
        }
        ProtocolExecution::Invoke(ExportDriver::ModuleAliasScopeCollision) => {
            run_module_alias_scope_collision_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::WideCallable) => {
            run_export_wide_callable_js(ctx, pkg_obj, protocol)
        }
        ProtocolExecution::Invoke(ExportDriver::Coexist) => {
            unreachable!("--protocol coexist dispatches through run_coexist")
        }
    }
}

fn run_default_main_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    module: &str,
) -> Result<(), String> {
    let main_fn = main_callable(ctx, pkg_obj, module)?;
    main_fn
        .call::<_, Value>(())
        .catch(ctx)
        .map_err(|e| format!("calling `{module}/main`: {}", format_caught(&e)))?;
    Ok(())
}

fn run_returned_forall_call_by_value_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let main = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const run = {main}.main;
  run();
  try {{
    run();
    throw new Error("produce did not throw");
  }} catch (error) {{
    if (!(error instanceof Error) || error.message !== "produce failed") throw error;
    __kio_host_print__("caught\n");
  }}
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running returned-forall-call-by-value: {}",
                format_caught(&e)
            )
        })
}

fn run_newtype_visibility_facade_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let types = export_base_js_expr(protocol, "types");
    let left = export_base_js_expr(protocol, "left");
    let right = export_base_js_expr(protocol, "right");
    let script = render_newtype_visibility_facade_js(&types, &left, &right);
    let driver: Function = ctx
        .eval(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("preparing newtype-visibility-facade: {}", format_caught(&e)))?;
    driver
        .call::<_, ()>(())
        .catch(ctx)
        .map_err(|e| format!("running newtype-visibility-facade: {}", format_caught(&e)))
}

fn render_newtype_visibility_facade_js(types: &str, left: &str, right: &str) -> String {
    let type_selector = |source| js_string_literal(&host_api::facade_type_selector(source));
    let constructor_only = type_selector("Constructor_only");
    let projector_only = type_selector("Projector_only");
    let both_public = type_selector("Both_public");
    let shared = type_selector("Shared");
    let constructor_pair = type_selector("Constructor_pair");
    let projector_pair = type_selector("Projector_pair");
    let constructor_generic = type_selector("Constructor_generic");
    let projector_generic = type_selector("Projector_generic");
    let packed_function = type_selector("Packed_function");
    let existential_unit = type_selector("Existential_unit");
    let existential_empty = type_selector("Existential_empty");
    let recursive_both = type_selector("Recursive_both");
    let recursive_constructor = type_selector("Recursive_constructor");
    let recursive_projector = type_selector("Recursive_projector");
    let constructor_spread = type_selector("Constructor_spread");
    let projector_spread = type_selector("Projector_spread");
    let existential_spread = type_selector("Existential_spread");
    let recursive_existential_function = type_selector("Recursive_existential_function");
    format!(
        r#"
(() => {{
  const types = {types};
  const left = {left};
  const right = {right};
  const print = value => {{
    if (typeof value !== "number") {{
      throw new Error("facade did not preserve the selected testapi.I32 representation");
    }}
    __kio_host_print__(String(value) + "\n");
  }};
  const printPair = value => {{
    if (typeof value !== "object"
        || value === null
        || typeof value._0 !== "number"
        || typeof value._1 !== "number") {{
      throw new Error("facade did not preserve the compound product representation");
    }}
    __kio_host_print__(String(value._0) + " " + String(value._1) + "\n");
  }};

  print(types.readA(types.makeA(11)));
  print(types.readB(types.makeB(22)));
  print(types.readConstructorOnlyValue(
    types[{constructor_only}].makeConstructorOnly(33)));
  print(types[{projector_only}].readProjectorOnly(
    types.makeProjectorOnlyValue(44)));
  print(types[{both_public}].readBothPublic(
    types[{both_public}].makeBothPublic(55)));
  print(left[{shared}].readShared(left[{shared}].makeShared(left.read(left.make(66)))));
  print(right[{shared}].readShared(right[{shared}].makeShared(right.read(right.make(77)))));

  const constructorPair = types[{constructor_pair}].makeConstructorPair(
    {{ _0: 81, _1: 82 }});
  const constructorPairOut = types.readConstructorPairValue(constructorPair);
  printPair(constructorPairOut);
  printPair(types[{projector_pair}].readProjectorPair(
    types.makeProjectorPairValue(constructorPairOut)));

  print(types.readConstructorGenericValue(
    types[{constructor_generic}].makeConstructorGeneric(85)));
  print(types[{projector_generic}].readProjectorGeneric(
    types.makeProjectorGenericValue(86)));

  const packed = types[{packed_function}].makePackedFunction(
    value => value._0 + value._1);
  const unpacked = types[{packed_function}].readPackedFunction(packed);
  print(unpacked(constructorPairOut));

  print(types[{existential_unit}].readExistentialUnit(
    types.makeExistentialUnitValue())(_value => 89));

  print(types[{existential_empty}].readExistentialEmpty(
    types.makeExistentialEmptyValue())((...args) => {{
      if (args.length !== 0) {{
        throw new Error("unit existential continuation received value arguments");
      }}
      return 90;
    }}));

  const recursiveBoth = types[{recursive_both}].makeRecursiveBoth(
    types.recursiveBothBasePayload());
  print(types.recursiveBothPayloadIsBase(
    types[{recursive_both}].readRecursiveBoth(recursiveBoth)));

  const recursiveConstructor = types[{recursive_constructor}].makeRecursiveConstructor(
    types.recursiveConstructorBasePayload());
  print(types.recursiveConstructorPayloadIsBase(
    types.readRecursiveConstructorValue(recursiveConstructor)));

  print(types.recursiveProjectorPayloadIsBase(
    types[{recursive_projector}].readRecursiveProjector(
      types.makeRecursiveProjectorBase())));
  const check = (actual, expected) => {{ if (actual !== expected) throw new Error("visibility callable changed"); }};
  const constructorInputs = [];
  const constructed = types[{constructor_spread}].makeConstructorSpread(value => {{
    constructorInputs.push(value._0); return value._0 + 3;
  }});
  check(types.invokeConstructorSpreadI32(constructed, 101), 104);
  check(types.invokeConstructorSpreadUnit(constructed, 102), 105);
  check(JSON.stringify(constructorInputs), "[101,102]");
  const projected = types[{projector_spread}].readProjectorSpread(types.makeProjectorSpreadValue());
  check(projected({{_0: 111, _1: 1}}), 111);
  check(projected({{_0: 112, _1: null}}), 112);
  let spreadOpens = 0, recursiveOpens = 0;
  const openSpread = value => {{ check(typeof value, "function"); spreadOpens++; return 91; }};
  const openRecursive = value => {{ check(typeof value, "function"); recursiveOpens++; return 92; }};
  check(types[{existential_spread}].readExistentialSpread(types.makeExistentialSpreadValue())(openSpread), 91);
  check(types[{recursive_existential_function}].readRecursiveExistentialFunction(types.makeRecursiveExistentialFunctionValue())(openRecursive), 92);
  check(spreadOpens, 1);
  check(recursiveOpens, 1);
}})
"#
    )
}

fn run_export_namespace_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The exported `main` / `utils` modules have semantic identities
    // `testapi/main` and `testapi/utils`; `export_base_js_expr` renders
    // their role-framed facade selectors.
    let main_ns = export_base_js_expr(protocol, "main");
    let utils_ns = export_base_js_expr(protocol, "utils");
    let script = format!(
        r#"
(() => {{
  const main_ns = {main_ns};
  const utils_ns = {utils_ns};
  if (main_ns === undefined || main_ns === null) {{
    throw new Error("export-namespace-roundtrip: missing main namespace");
  }}
  if (utils_ns === undefined || utils_ns === null) {{
    throw new Error("export-namespace-roundtrip: missing utils namespace");
  }}
  if (typeof main_ns.answer !== "function") {{
    throw new Error("export-namespace-roundtrip: missing main.answer");
  }}
  if (typeof utils_ns.echo !== "function") {{
    throw new Error("export-namespace-roundtrip: missing utils.echo");
  }}

  __kio_host_print__(String(main_ns.answer()) + "\n");
  __kio_host_print__(String(utils_ns.echo("namespace-utils")) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-namespace-roundtrip: {}", format_caught(&e)))
}

fn run_export_callback_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The callback fns are exported in semantic module `testapi/main`.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main = {base};
  if (typeof main.applyTwice !== "function") {{
    throw new Error("export-callback-roundtrip: missing exported fn applyTwice");
  }}
  if (typeof main.makeStep !== "function") {{
    throw new Error("export-callback-roundtrip: missing exported fn makeStep");
  }}

  __kio_host_print__(String(main.applyTwice((n) => n + 3, 10)) + "\n");
  const step = main.makeStep(4);
  if (typeof step !== "function") {{
    throw new Error("export-callback-roundtrip: makeStep returned non-function");
  }}
  __kio_host_print__(String(step(5)) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-callback-roundtrip: {}", format_caught(&e)))
}

fn run_nested_curried_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let api = export_base_js_expr(protocol, "api");
    let script = format!(
        r#"
(() => {{
  const api = {api};
  const join = left => right => left + "/" + right;
  const viaHost = api.viaHost(join);
  if (typeof viaHost !== "function") {{
    throw new Error("nested-curried-roundtrip: viaHost returned non-function");
  }}
  const viaHostSecond = viaHost("env-left");
  if (typeof viaHostSecond !== "function") {{
    throw new Error("nested-curried-roundtrip: viaHost flattened the second layer");
  }}
  __kio_host_print__("via host: " + viaHostSecond("env-right") + "\n");

  const roundExport = api.roundExport(join);
  if (typeof roundExport !== "function") {{
    throw new Error("nested-curried-roundtrip: roundExport returned non-function");
  }}
  const roundExportSecond = roundExport("export-left");
  if (typeof roundExportSecond !== "function") {{
    throw new Error("nested-curried-roundtrip: roundExport flattened the second layer");
  }}
  __kio_host_print__("round export: " + roundExportSecond("export-right") + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running nested-curried-roundtrip: {}", format_caught(&e)))
}

fn run_host_substituted_unit_callback_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let api = export_base_js_expr(protocol, "api");
    let script = format!(
        r#"
(() => {{
  const api = {api};
  __kio_host_print__(api.viaHost((_unit) => "callback") + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running host-substituted-unit-callback: {}",
                format_caught(&e)
            )
        })
}

fn run_export_newtype_ignored_argument_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let main = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main = {main};
  __kio_host_print__(String(main.toI32(main.fromI32(7))) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-newtype-ignored-argument-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn run_recursive_newtype_boundary_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let main = export_base_js_expr(protocol, "main");
    let root_type = js_string_literal(&host_api::facade_type_selector("Root"));
    let script = format!(
        r#"
(() => {{
  const main = {main};
  const rootType = main[{root_type}];
  const payload = main.basePayload();
  const root = rootType.makeRoot(payload);
  const kept = main.keep(root);
  const projected = rootType.readRoot(kept);
  __kio_host_print__(String(main.acceptPayload(projected)) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running recursive-newtype-boundary: {}", format_caught(&e)))
}

fn run_facade_selector_collisions_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let api = export_base_js_expr(protocol, "api");
    let foo_module = export_base_js_expr(protocol, "foo");
    let foo_bar = export_base_js_expr(protocol, "foo_bar");
    let i = export_base_js_expr(protocol, "i");
    let host = export_base_js_expr(protocol, "host");
    let mod_api_value = export_base_js_expr(protocol, "mod_api_value");
    let bar_module = js_string_literal(&host_api::facade_module_selector("bar", false));
    let child_module = js_string_literal(&host_api::facade_module_selector("child", false));
    let child_type = js_string_literal(&host_api::facade_type_selector("Child"));
    let script = format!(
        r#"
(() => {{
  const api = {api};
  const foo = {foo_module};
  const fooBar = {foo_bar};
  const i = {i};
  api.pkg();
  api.value();
  {host}.value();
  {mod_api_value}.value();
  __kio_host_print__(String(foo[{bar_module}].value(9, 1)) + "\n");
  __kio_host_print__(String(fooBar.value(10, 2)) + "\n");
  __kio_host_print__(String(i.value(41, 1)) + "\n");
  api.child();
  __kio_host_print__("api.child function\n");
  api[{child_module}].value();
  __kio_host_print__("api/child module\n");
  const child = api[{child_type}].makeChild(30);
  api[{child_type}].readChild(child);
  __kio_host_print__("api.Child type\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running facade-selector-collisions: {}", format_caught(&e)))
}

fn run_public_word_names_js<'js>(ctx: &Ctx<'js>, pkg_obj: &Object<'js>) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let script = r#"
(() => {
  const api = globalThis.__kio_protocol_pkg__.wordApi;
  const nodes = api.KioModule_wordNodes;
  const box = nodes.KioType__uWordBox_u_u;
  const boxed = box.wrapWord(55);
  if (boxed._WordBox__ !== 55 || Object.keys(boxed).join() !== "_WordBox__")
    throw new Error("incorrect public transparent wrapper key");
  const pair = api.keepPair({
    _WordBox__: 77,
    "wordApi/otherNodes._WordBox__": 88
  });
  const values = [api.readWord(), api._readWord(), api.readWord_(),
    api._readWord_(), api.readWord__(), box.unwrapWord({_WordBox__: 55}),
    nodes.keepWord(66), pair._WordBox__, pair["wordApi/otherNodes._WordBox__"]];
  for (const value of values) __kio_host_print__(String(value) + "\n");
})()
"#;
    ctx.eval::<(), _>(script.as_bytes())
        .catch(ctx)
        .map_err(|e| format!("running public-word-names: {}", format_caught(&e)))
}

fn run_module_alias_scope_collision_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let a = export_base_js_expr(protocol, "a");
    let b = export_base_js_expr(protocol, "b");
    let script = format!(
        r#"
(() => {{
  const a = {a};
  const b = {b};
  a.consume(a.make());
  b.consume(b.make());
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running module-alias-scope-collision: {}",
                format_caught(&e)
            )
        })
}

fn run_export_module_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The exported `api` module has semantic identity `testapi/api`.
    let base = export_base_js_expr(protocol, "api");
    let script = format!(
        r#"
(() => {{
  const api = {base};
  if (api === undefined || api === null) {{
    throw new Error("export-module-roundtrip: missing api namespace");
  }}
  if (typeof api.tag !== "function") {{
    throw new Error("export-module-roundtrip: missing api.tag");
  }}
  if (typeof api.value !== "function") {{
    throw new Error("export-module-roundtrip: missing api.value");
  }}
  if (typeof api.echo !== "function") {{
    throw new Error("export-module-roundtrip: missing api.echo");
  }}

  __kio_host_print__(String(api.tag()) + "\n");
  __kio_host_print__(String(api.value()) + "\n");
  __kio_host_print__(String(api.echo("module-echo")) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-module-roundtrip: {}", format_caught(&e)))
}

fn run_export_multilabel_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The exported `say` fn is in `testapi/main`. Its multi-label product param keys the
    // label-newtypes `A` / `B` (the surface `pub labels { a, b }` mint
    // names) per `specs/backends/js.md` § Structural and nominal types.
    let base = export_base_js_expr(protocol, "main");
    let script = render_export_multilabel_roundtrip_js(&base);
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-multi-label-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn render_export_multilabel_roundtrip_js(base: &str) -> String {
    let a = js_string_literal(&host_api::facade_type_selector("A"));
    format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.say !== "function") {{
    throw new Error("export-multi-label-roundtrip: missing exported fn say");
  }}

  main_ns.say({{ A: 42, B: "shown\n" }});
  const row = main_ns.echoPair({{ A: 88, B: "99" }});
  __kio_host_print__(String(row.A) + "\n");
  __kio_host_print__(String(row.B) + "\n");
  __kio_host_print__(String(main_ns[{a}].get(main_ns.echoA(main_ns[{a}].mk(111)))) + "\n");
}})()
"#
    )
}

fn run_export_poly_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The polymorphic fns are exported at the testapi root
    // (`pkg.testapi.poly_echo` / `.keep_left`); module `testapi`.
    let base = export_root_base_js_expr(protocol);
    let script = format!(
        r#"
(() => {{
  const root = {base};
  if (typeof root.polyEcho !== "function") {{
    throw new Error("export-poly-roundtrip: missing exported fn polyEcho");
  }}
  if (typeof root.keepLeft !== "function") {{
    throw new Error("export-poly-roundtrip: missing exported fn keepLeft");
  }}

  __kio_host_print__(String(root.polyEcho("poly-string")) + "\n");
  __kio_host_print__(String(root.polyEcho(42)) + "\n");
  __kio_host_print__(String(root.keepLeft("left", 99)) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-poly-roundtrip: {}", format_caught(&e)))
}

fn run_host_existential_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let main = export_base_js_expr(protocol, "main");
    let types = export_base_js_expr(protocol, "types");
    let packed = js_string_literal(&host_api::facade_type_selector("Packed"));
    let script = format!(
        r#"(() => {{
  let observations = 0, openings = 0;
  globalThis.__kio_existential_observe__ = value => {{
    observations++;
    return {types}[{packed}].readPacked(value)(payload => {{
      openings++;
      return payload._1(payload._0);
    }});
  }};
  const result = {main}.exercise();
  if (result._0 !== 37 || result._1 !== 83 || observations !== 2 || openings !== 2)
    throw new Error("existential host observations changed");
  __kio_host_print__("existential host opening ok\n");
}})()"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running host-existential-roundtrip: {}", format_caught(&e)))
}

fn run_export_functor_dict_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let main = export_base_js_expr(protocol, "main");
    let types = export_base_js_expr(protocol, "types");
    let box_type = js_string_literal(&host_api::facade_type_selector("Box"));
    let functor_type = js_string_literal(&host_api::facade_type_selector("Functor"));
    let script = format!(
        r#"(() => {{
  const main = {main};
  const box = {types}[{box_type}];
  const functor = {types}[{functor_type}];
  const integers = [], texts = [];
  const toText = value => {{ integers.push(value); return "v:" + value; }};
  const toInteger = value => {{ texts.push(value); return value.length; }};
  const check = (actual, expected) => {{ if (actual !== expected) throw new Error("functor payload changed"); }};
  const wrap = value => box.mkBox({{_0: value, _1: null}});
  const unwrap = value => {{ const row = box.unBox(value); check(row._1, null); return row._0; }};
  const dict = main.echoFunctor(main.boxFunctor());
  check(unwrap(main.applyFunctor(dict, toText, wrap(42))), "v:42");
  check(unwrap(main.applyFunctor(dict, toInteger, wrap("apple"))), 5);
  const map = functor.fmap(dict);
  check(unwrap(map({{_0: toText, _1: wrap(7)}})), "v:7");
  check(unwrap(map({{_0: toInteger, _1: wrap("pear")}})), 4);
  check(JSON.stringify(integers), "[42,7]");
  check(JSON.stringify(texts), '["apple","pear"]');
  __kio_host_print__("functor dictionary ok\n");
}})()"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-functor-dict-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn run_export_callable_slots_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"(() => {{
  const main = {base};
  const check = (actual, expected) => {{
    if (actual !== expected) throw new Error("callable slot payload changed");
  }};
  let productCalls = 0;
  const productStep = value => {{ productCalls++; return value + 5; }};
  check(main.applyProduct({{_0: productStep, _1: 11}}), 16);
  const echoedProduct = main.echoProduct({{_0: productStep, _1: 17}});
  check(echoedProduct._1, 17);
  check(echoedProduct._0(echoedProduct._1), 22);
  const madeProduct = main.makeProduct(23);
  check(madeProduct._1, 23);
  check(madeProduct._0(29), 29);
  let sumCalls = 0;
  const sumStep = value => {{ sumCalls++; return value + 7; }};
  check(main.applySum({{_0: sumStep}}, 31), 38);
  const echoedSum = main.echoSum({{_0: sumStep}});
  check(Object.keys(echoedSum).join(), "_0");
  check(echoedSum._0(37), 44);
  const madeSum = main.makeCallableSum();
  check(Object.keys(madeSum).join(), "_0");
  check(madeSum._0(41), 41);
  const scalar = main.makeScalarSum(97);
  check(Object.keys(scalar).join(), "_1");
  check(scalar._1, 97);
  check(main.applySum(scalar, 43), 97);
  const echoedScalar = main.echoSum(scalar);
  check(Object.keys(echoedScalar).join(), "_1");
  check(echoedScalar._1, 97);
  check(productCalls, 2);
  check(sumCalls, 2);
  __kio_host_print__("callable slots ok\n");
}})()"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-callable-slots-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn run_export_native_payload_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let base = export_base_js_expr(protocol, "main");
    let body = match protocol.contract().execution {
        ProtocolExecution::Invoke(ExportDriver::ScalarRoundtrip) => {
            r#"
  const samples = [
    [main.echoI128, -1208925819614629174706299n],
    [main.echoU128, 2417851639229258349412391n],
    [main.echoF32, 1.5], [main.echoF32, -2.25],
    [main.echoF64, 1.0000000000000002], [main.echoF64, -3.125],
  ];
  for (const [echo, value] of samples) {
    if (echo(value) !== value) throw new Error("scalar payload changed");
  }
  __kio_host_print__("scalar payloads ok\n");
"#
        }
        ProtocolExecution::Invoke(ExportDriver::HostOwnedRoundtrip) => {
            r#"
  for (const value of [7, 19]) {
    const returned = main.echoToken({value});
    if (returned.value !== value) throw new Error("token payload changed");
  }
  for (const value of [42, "box-value"]) {
    const returned = main.echoBox({value});
    if (returned.value !== value) throw new Error("box payload changed");
  }
  __kio_host_print__("host-owned payloads ok\n");
"#
        }
        _ => unreachable!("native payload helper only handles its two fixed drivers"),
    };
    let script = format!("(() => {{ const main = {base}; {body} }})()");
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running {}: {}", protocol.name(), format_caught(&e)))
}

fn run_export_structural_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The exported `pair_swap` / `dispatch_left` fns are in
    // `testapi/main`. `pair_swap`
    // takes / returns a `{_0,_1}` product; `dispatch_left` takes a
    // `{_0}|{_1}` sum per `specs/backends/js.md` § Structural and nominal
    // types.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.pairSwap !== "function") {{
    throw new Error("export-structural-roundtrip: missing exported fn pairSwap");
  }}
  if (typeof main_ns.dispatchLeft !== "function") {{
    throw new Error("export-structural-roundtrip: missing exported fn dispatchLeft");
  }}
  if (typeof main_ns.rotate !== "function") {{
    throw new Error("export-structural-roundtrip: missing exported fn rotate");
  }}
  if (typeof main_ns.classify !== "function") {{
    throw new Error("export-structural-roundtrip: missing exported fn classify");
  }}
  if (typeof main_ns.chooseFirst !== "function" || typeof main_ns.chooseMiddle !== "function" || typeof main_ns.chooseLast !== "function") {{
    throw new Error("export-structural-roundtrip: missing exported chooser");
  }}

  const q = main_ns.pairSwap({{ _0: 42, _1: "hello" }});
  __kio_host_print__(String(q._0) + " " + String(q._1) + "\n");
  __kio_host_print__(String(main_ns.dispatchLeft({{ _0: 7 }})) + "\n");
  __kio_host_print__(String(main_ns.dispatchLeft({{ _1: "from-sum" }})) + "\n");
  const wide = main_ns.rotate({{ _0: 1, _1: 2, _2: 3, _3: 4, _4: 5, _5: 6, _6: 7, _7: 8, _8: 9, _9: 10, _10: 11, _11: 12 }});
  __kio_host_print__(String(wide._0) + " " + String(wide._1) + " " + String(wide._11) + "\n");
  __kio_host_print__(String(main_ns.classify({{ _0: 1 }})) + "\n");
  __kio_host_print__(String(main_ns.classify({{ _4: 5 }})) + "\n");
  __kio_host_print__(String(main_ns.classify({{ _9: "ten" }})) + "\n");
  __kio_host_print__(String(main_ns.classify(main_ns.chooseFirst())) + "\n");
  __kio_host_print__(String(main_ns.classify(main_ns.chooseMiddle())) + "\n");
  __kio_host_print__(String(main_ns.classify(main_ns.chooseLast())) + "\n");
  const samples = [
    [0, -101], [1, -12345], [2, -123456789], [3, -9007199254740993n],
    [4, 201], [5, 54321], [6, 3456789012], [7, 18014398509481987n],
    [8, false], [8, true], [9, "sum-value"],
  ];
  for (const [arm, value] of samples) {{
    const key = "_" + arm;
    const returned = main_ns.echoSum({{ [key]: value }});
    if (Object.keys(returned).length !== 1 || !Object.hasOwn(returned, key) || returned[key] !== value) {{
      throw new Error("export-structural-roundtrip: sum payload changed at arm " + arm);
    }}
    __kio_host_print__(String(main_ns.classify(returned)) + " " + String(returned[key]) + "\n");
  }}
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-structural-roundtrip: {}", format_caught(&e)))
}

fn run_export_positional_product_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // Flat (not testapi-conformed) golden. `make_pair(I32, String) ->
    // I32 & String` is exported at the bulk-flat `pkg.main`. Call it and
    // read the returned product back through its positional `_0` / `_1`
    // fields per `specs/backends/js.md` § Structural and nominal types —
    // the same positional FFI field names the Rust side relies on.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.makePair !== "function") {{
    throw new Error("export-positional-product-roundtrip: missing exported fn makePair");
  }}

  const q = main_ns.makePair(7, "hello");
  __kio_host_print__(String(q._0) + " " + String(q._1) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-positional-product-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn run_export_type_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // The exported `Pair` handle sits under the role-framed `types`
    // namespace at the testapi package surface.
    let base = export_base_js_expr(protocol, "types");
    let pair = js_string_literal(&host_api::facade_type_selector("Pair"));
    let script = format!(
        r#"
(() => {{
  const types = {base};
  if (types === undefined || types === null || types[{pair}] === undefined || types[{pair}] === null) {{
    throw new Error("export-type-roundtrip: missing exported type Pair");
  }}
  const Pair = types[{pair}];
  if (typeof Pair.mkPair !== "function") {{
    throw new Error("export-type-roundtrip: missing Pair.mkPair");
  }}
  if (typeof Pair.unPair !== "function") {{
    throw new Error("export-type-roundtrip: missing Pair.unPair");
  }}

  const boxed = Pair.mkPair({{ _0: "export-type-left", _1: "export-type-right" }});
  const payload = Pair.unPair(boxed);
  __kio_host_print__(String(payload._0) + "\n");
  __kio_host_print__(String(payload._1) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-type-roundtrip: {}", format_caught(&e)))
}

fn run_export_newtype_sum_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // `pack(I32, String) -> Tagged` (Out) and `first_or(I32, Tagged) ->
    // I32` (In) are in `testapi/main`,
    // where `Tagged = Pr | .` and `Pr` wraps `(I32 & String)`. Chaining
    // them round-trips a sum-arm-over-a-newtype-over-a-product; the
    // recovered first field prints `7`.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.pack !== "function") {{
    throw new Error("export-newtype-sum-roundtrip: missing exported fn pack");
  }}
  if (typeof main_ns.firstOr !== "function") {{
    throw new Error("export-newtype-sum-roundtrip: missing exported fn firstOr");
  }}

  __kio_host_print__(String(main_ns.firstOr(0, main_ns.pack(7, "hi"))) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-newtype-sum-roundtrip: {}",
                format_caught(&e)
            )
        })
}

/// Drive the `export-curried-facade` protocol: the package exports
/// `pick(a: Str)(b: Str) -> Str` and `last(a: I32)(b: I32, c: I32) -> I32`;
/// the host surface flattens the value groups into one call
/// (`specs/backends/README.md` § Function-type FFI canonicalization), so
/// the driver calls both flat and prints the routed-through values.
fn run_export_curried_facade_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.pick !== "function") {{
    throw new Error("export-curried-facade: missing exported fn pick");
  }}

  __kio_host_print__(main_ns.pick("ku", "rz") + "\n");
  __kio_host_print__(String(main_ns.last(1, 2, 3)) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-curried-facade: {}", format_caught(&e)))
}

fn run_export_wide_callable_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.select !== "function" || typeof main_ns.makeSelect !== "function") {{
    throw new Error("export-wide-callable: missing exported fn");
  }}

  const values = Array.from({{ length: {WIDE_CALLABLE_SLOT_COUNT} }}, (_value, index) => index);
  const out = main_ns.select(...values);
  __kio_host_print__(String(out._0) + "\n");
  __kio_host_print__(String(out._1) + "\n");
  __kio_host_print__(String(out._2) + "\n");
  const callback = main_ns.makeSelect();
  if (typeof callback !== "function") {{
    throw new Error("export-wide-callable: makeSelect returned a non-callable value");
  }}
  const callback_args = Object.fromEntries(values.map((value, index) => ["_" + index, value]));
  const callback_out = callback(callback_args);
  __kio_host_print__(String(callback_out._0) + "\n");
  __kio_host_print__(String(callback_out._1) + "\n");
  __kio_host_print__(String(callback_out._2) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-wide-callable: {}", format_caught(&e)))
}

fn run_export_poly_callback_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // `apply_via[K][R](f: K -> R, x: K) -> R` is in `testapi/main`. Pass a
    // host closure at two concrete instantiations — a string transform and
    // an integer step — and print each result.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.applyVia !== "function") {{
    throw new Error("export-poly-callback-roundtrip: missing exported fn applyVia");
  }}

  __kio_host_print__(main_ns.applyVia((s) => "via: " + s, "apply") + "\n");
  __kio_host_print__(String(main_ns.applyVia((n) => n + 8, 7)) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-poly-callback-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn run_compound_input_once_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const api = {base};
  const print = value => __kio_host_print__(String(value) + "\n");
  const equal = (actual, expected) => {{
    if (JSON.stringify(actual) !== JSON.stringify(expected)) {{
      throw new Error("compound-input-once: " + JSON.stringify(actual) + " != " + JSON.stringify(expected));
    }}
  }};
  const direct = api.direct();
  print(direct._0); print(direct._1);
  const callback = api.callback(() => {{ print("callback"); return {{ _0: 9, _1: "callback-value" }}; }});
  print(callback._0); print(callback._1);

  let reads = [];
  const field = (object, key, label, value) => Object.defineProperty(object, key, {{
    enumerable: true, get() {{ reads.push(label); return value; }}
  }});
  const inner = (left, right) => field(field({{}}, "_0", "left", left), "_1", "right", right);
  const outer = (text, left, right) => field(field({{}}, "_0", "text", text), "Inner", "Inner", inner(left, right));
  const input = field({{}}, "Outer", "Outer", outer("nest", 11, 13));
  const nested = api.echoOuter(input).Outer;
  equal(reads, ["Outer", "text", "Inner", "left", "right"]);
  print(nested._0); print(nested.Inner._0); print(nested.Inner._1);
  equal(api.echoOuter(api.makeOuter("nest", 11, 13)), {{ Outer: {{ _0: "nest", Inner: {{ _0: 11, _1: 13 }} }} }});

  for (const [key, payload, expectedReads, expected] of [
    ["_0", 17, ["Choice", "selected"], {{ Choice: {{ _0: 17 }} }}],
    ["Inner", inner(19, 23), ["Choice", "selected", "left", "right"], {{ Choice: {{ Inner: {{ _0: 19, _1: 23 }} }} }}],
    ["Outer", outer("choice", 29, 31), ["Choice", "selected", "text", "Inner", "left", "right"], {{ Choice: {{ Outer: {{ _0: "choice", Inner: {{ _0: 29, _1: 31 }} }} }} }}],
  ]) {{
    reads = [];
    const target = field({{}}, key, "selected", payload);
    equal(Object.keys(target), [key]);
    // The sum has exactly one key. Membership stays truthful; only an actual
    // read of an absent alternative fails, independently of discriminator probes.
    const oneKey = new Proxy(target, {{ get(object, name, receiver) {{
      if (["_0", "Inner", "Outer"].includes(name) && !Reflect.has(object, name)) {{
        throw new Error("compound-input-once: read unselected " + name);
      }}
      return Reflect.get(object, name, receiver);
    }} }});
    const out = api.echoChoice(field({{}}, "Choice", "Choice", oneKey));
    equal(reads, expectedReads);
    equal(out, expected);
    const value = out.Choice[key];
    if (key === "_0") print(value);
    else if (key === "Inner") {{ print(value._0); print(value._1); }}
    else {{ print(value._0); print(value.Inner._0); print(value.Inner._1); }}
  }}
  equal(api.echoChoice(api.first(17)), {{ Choice: {{ _0: 17 }} }});
  equal(api.echoChoice(api.middle(19, 23)), {{ Choice: {{ Inner: {{ _0: 19, _1: 23 }} }} }});
  equal(api.echoChoice(api.last("choice", 29, 31)), {{ Choice: {{ Outer: {{ _0: "choice", Inner: {{ _0: 29, _1: 31 }} }} }} }});
  print(api.echoText("atomic"));
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| format!("running export-compound-input-once: {}", format_caught(&e)))
}

fn run_export_nested_product_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // `make(String, I32, I32) -> Outer` is in `testapi/main`, where the label-minted `Inner` wraps
    // `(I32 & I32)` and `Outer` wraps `(String & Inner)`. The return
    // crosses as `{ Outer: { _0, Inner: { _0, _1 } } }` — the newtype
    // envelope, positional keys for the plain slots, the bare newtype
    // name for the nested newtype slot. Each product level's Out
    // conversion opens its own binding scope, so the call regresses the
    // nested-binder shadowing the JS skin once emitted (a
    // temporal-dead-zone `ReferenceError`). Print all three leaf slots.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.make !== "function") {{
    throw new Error("export-nested-product-roundtrip: missing exported fn make");
  }}

  const out = main_ns.make("nest", 7, 9);
  __kio_host_print__(String(out.Outer._0) + "\n");
  __kio_host_print__(String(out.Outer.Inner._0) + "\n");
  __kio_host_print__(String(out.Outer.Inner._1) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-nested-product-roundtrip: {}",
                format_caught(&e)
            )
        })
}

fn run_export_newtype_scalar_roundtrip_js<'js>(
    ctx: &Ctx<'js>,
    pkg_obj: &Object<'js>,
    protocol: RunnerProtocol,
) -> Result<(), String> {
    ctx.globals()
        .set("__kio_protocol_pkg__", pkg_obj.clone())
        .map_err(|e| format!("installing protocol package binding: {e}"))?;
    // `bump(Wrap) -> Wrap` is in `testapi/main`, where `Wrap` is a bare scalar-payload
    // newtype (`newtype Wrap : I32`). A newtype crosses the JS FFI as the
    // boundary record `{ Wrap: <payload> }` (the named-newtype envelope
    // the export wrapper packs / unpacks), so pass `{ Wrap: 7 }` and read
    // `.Wrap` off the returned record; the payload round-trips to `7`.
    let base = export_base_js_expr(protocol, "main");
    let script = format!(
        r#"
(() => {{
  const main_ns = {base};
  if (main_ns === undefined || main_ns === null || typeof main_ns.bump !== "function") {{
    throw new Error("export-newtype-scalar-roundtrip: missing exported fn bump");
  }}

  const out = main_ns.bump({{ Wrap: 7 }});
  __kio_host_print__(String(out.Wrap) + "\n");
}})()
"#
    );
    ctx.eval::<(), _>(script.into_bytes())
        .catch(ctx)
        .map_err(|e| {
            format!(
                "running export-newtype-scalar-roundtrip: {}",
                format_caught(&e)
            )
        })
}

/// A JS expression navigating from `globalThis.__kio_protocol_pkg__`
/// to the exported sub-namespace `<leaf>` under the export root: for a
/// testapi protocol that is `pkg.testapi.<leaf>`, else the bulk flat
/// `pkg.<leaf>`.
fn export_base_js_expr(protocol: RunnerProtocol, leaf: &str) -> String {
    let selector = host_api::facade_module_selector(leaf, testapi_export_root(protocol).is_none());
    let selector = js_string_literal(&selector);
    match testapi_export_root(protocol) {
        Some(root) => format!(
            "globalThis.__kio_protocol_pkg__[{}][{selector}]",
            js_string_literal(&root)
        ),
        None => format!("globalThis.__kio_protocol_pkg__[{selector}]"),
    }
}

/// The export root for a testapi-conformed protocol (the fixed
/// `testapi` namespace) or the elab POC (its own `elab` namespace),
/// else `None` for the bulk goldens.
fn testapi_export_root(protocol: RunnerProtocol) -> Option<String> {
    protocol.export_root()
}

/// A JS expression navigating from `globalThis.__kio_protocol_pkg__` to
/// the export root *itself* (where root-module `pub fn`s are exposed):
/// `pkg.testapi` for a testapi protocol, else the bulk-flat `pkg`.
fn export_root_base_js_expr(protocol: RunnerProtocol) -> String {
    match testapi_export_root(protocol) {
        Some(root) => format!(
            "globalThis.__kio_protocol_pkg__[{}]",
            js_string_literal(&root)
        ),
        None => "globalThis.__kio_protocol_pkg__".to_owned(),
    }
}

/// Resolve the protocol's exact main module and require its `main` export.
fn main_callable<'js>(
    ctx: &Ctx<'js>,
    pkg: &Object<'js>,
    module: &str,
) -> Result<Function<'js>, String> {
    let mut current = pkg.clone();
    for (index, segment) in module.split('/').enumerate() {
        let selector = host_api::facade_module_selector(segment, index == 0);
        let value: Value<'js> = current
            .get(selector)
            .catch(ctx)
            .map_err(|e| format!("reading main module `{module}`: {}", format_caught(&e)))?;
        current = value
            .into_object()
            .ok_or_else(|| format!("main module `{module}` is missing or not an object"))?;
    }
    let main: Value<'js> = current
        .get("main")
        .catch(ctx)
        .map_err(|e| format!("reading main export `{module}/main`: {}", format_caught(&e)))?;
    main.as_function()
        .cloned()
        .ok_or_else(|| format!("main export `{module}/main` is missing or not callable"))
}

/// Register the I/O / process callables (`print`, `eprint`, `exit`)
/// on the global object. The arrow functions inside the host record
/// reference these by name (`__kio_host_print__`, etc.) — since they
/// resolve through the global scope, the host record can be a plain
/// JS object passed to the factory without needing a closure
/// over Rust-native references.
///
/// The i32 numeric items (`add_i32`, `i32_to_string`, …) live
/// entirely in the JS host record itself (built by
/// `build_host_record_expression`) and wrap through `BigInt.asIntN`.
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

/// Format a caught JS error for display. `CaughtError` wraps both
/// thrown values and structural errors; rendering via Display is
/// sufficient for the runner's diagnostic shape.
fn format_caught(err: &rquickjs::CaughtError<'_>) -> String {
    err.to_string()
}

/// Build the JS expression that produces the runner's default host
/// record. The result is a single expression — no top-level statement
/// outside the IIFE — so the caller can pass it to `Ctx::eval` and
/// receive the record as an `Object`.
///
/// The emitted package reads each host item at
/// `__host__.<MODULE_NS>.<leaf>` (nested by the declaring module's JS
/// namespace). Each host fn is nested under the module named by its
/// exact protocol binding (`TESTAPI_IO`, `TESTAPI_ARITH`, … after JS
/// namespace encoding). The module assignment is never inferred from a
/// leaf and is never read from a golden's `run.args`, source, or build
/// output. A flat binding keeps a top-level key. The JS engine is the
/// checker: a namespace / leaf mismatch surfaces as the package's
/// "missing host item" throw.
fn build_host_record_expression(host: &HostApi, protocol: RunnerProtocol) -> String {
    let contract = protocol.contract();
    host_api::assert_host_api(contract, host);
    let mut out = String::new();
    out.push_str("(() => {\n");
    out.push_str("  return ({\n");
    // Each host item nests under its declaring module's JS namespace,
    // matching the package's `__host__.<NS>.<leaf>` lookup. Resolve the
    // declaring module directly from the projected structured
    // identity. Decoding `<module>__<leaf>` from a flattened spelling would
    // be ambiguous when either source identifier legally contains `__`.
    let mut groups = std::collections::BTreeMap::<
        Option<String>,
        Vec<(&host_api::HostFunctionApi, &HostFnBinding)>,
    >::new();
    for (entry, binding) in host.functions.iter().zip(contract.host_fns) {
        assert_eq!(entry.rendered.name, entry.identity.leaf);
        for role in binding.body.role_refs().into_iter().flatten() {
            role.resolve(contract.host_types);
        }
        let ns = (!entry.identity.module.is_empty())
            .then(|| host_api::js_host_namespace(&entry.identity.module));
        groups.entry(ns).or_default().push((entry, binding));
    }
    for (ns, entries) in &groups {
        match ns {
            Some(ns) => {
                out.push_str("    ");
                out.push_str(&js_string_literal(ns));
                out.push_str(": ({\n");
                for (entry, binding) in entries {
                    push_js_host_fn_entry(&mut out, "      ", entry, binding);
                }
                out.push_str("    }),\n");
            }
            None => {
                for (entry, binding) in entries {
                    push_js_host_fn_entry(&mut out, "    ", entry, binding);
                }
            }
        }
    }
    out.push_str("  });\n");
    out.push_str("})()");
    out
}

fn push_js_host_fn_entry(
    out: &mut String,
    indent: &str,
    entry: &host_api::HostFunctionApi,
    binding: &HostFnBinding,
) {
    out.push_str(indent);
    // The record key is the host item's **bare leaf** (`print`,
    // `add_i32`) — the JS package nests these under the module
    // namespace, so the key carries no module prefix.
    out.push_str(&js_string_literal(&host_api::host_name_core(
        &entry.identity.leaf,
    )));
    out.push_str(": ");
    out.push_str(&render_js_host_fn_value(binding));
    out.push_str(",\n");
}

fn render_js_host_fn_value(binding: &HostFnBinding) -> String {
    match binding.body {
        HostFnBodyKind::CallStep { .. } => {
            "(step, seed) => step({ _0: seed, _1: 'compound-callback', _2: true })".to_owned()
        }
        HostFnBodyKind::MakePairCallback { .. } => {
            "(build, seed) => build(seed)._0".to_owned()
        }
        HostFnBodyKind::MakeStep { .. } => {
            "(delta) => (n) => Number(BigInt.asIntN(32, BigInt(n) + BigInt(delta)))".to_owned()
        }
        HostFnBodyKind::BoxMake { .. } => "(v) => ({ value: v })".to_owned(),
        HostFnBodyKind::BoxGet { .. } => "(box) => box.value".to_owned(),
        HostFnBodyKind::ApplyPoly { .. } => "(f) => f('rank-n\\n')".to_owned(),
        HostFnBodyKind::MakePairStructural { .. } => {
            "(n, s) => ({ _0: n, _1: s })".to_owned()
        }
        HostFnBodyKind::ProducePair { .. } => {
            "() => { __kio_host_print__('direct\\n'); return { _0: 7, _1: 'direct-value' }; }".to_owned()
        }
        HostFnBodyKind::SumToString { .. } => {
            "(v) => ('_0' in v ? String(v._0) : v._1)".to_owned()
        }
        HostFnBodyKind::MakeToken { .. } => "(n) => ({ value: n })".to_owned(),
        HostFnBodyKind::TokenValue { .. } => "(token) => token.value".to_owned(),
        HostFnBodyKind::RoundFunctor | HostFnBodyKind::RoundPicker => {
            // The polymorphic-function newtype crosses as an ordinary value;
            // hand the exact boundary value straight back.
            "(d) => d".to_owned()
        }
        HostFnBodyKind::ObservePacked { .. } => "value => globalThis.__kio_existential_observe__(value)".to_owned(),
        HostFnBodyKind::RoundPolyThunk => {
            "(thunk) => { const payload = thunk.PolyThunk; \
             if (typeof payload !== 'function' || payload.length !== 0) \
             throw new Error('round_poly_thunk: expected Poly_thunk payload callable arity 0'); \
             return thunk; }"
                .to_owned()
        }
        HostFnBodyKind::RoundPolyUnitSlot => {
            "(step) => { const payload = step.UnitSlot; \
             if (typeof payload !== 'function' || payload.length !== 1) \
             throw new Error('round_unit_slot: expected Unit_slot payload callable arity 1'); \
             return step; }"
                .to_owned()
        }
        HostFnBodyKind::StagedSecond { .. } => "(_first, second) => second".to_owned(),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            "(f) => { __kio_host_print__('round host probe: ' + f('host-left')('host-right') + '\\n'); return f; }".to_owned()
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            "(callback) => 'host/' + callback(null)".to_owned()
        }
        HostFnBodyKind::ReturnedForallUnit => {
            "(() => { let produced = false; return () => { if (produced) { __kio_host_print__('throw\\n'); throw new Error('produce failed'); } produced = true; __kio_host_print__('produce\\n'); return null; }; })()".to_owned()
        }
        HostFnBodyKind::TraceUnit { text } => format!(
            "() => {{ __kio_host_print__({} + '\\n'); }}",
            js_string_literal(text)
        ),
        HostFnBodyKind::StagedUnitCall => {
            "(_value) => { __kio_host_print__('staged Unit host call\\n'); }".to_owned()
        }
        HostFnBodyKind::UnreachableI32Print { .. } => {
            "(_n) => { throw new Error('unreachable host function'); }".to_owned()
        }
        HostFnBodyKind::Print { .. } => "(s) => __kio_host_print__(s)".to_owned(),
        HostFnBodyKind::Eprint { .. } => "(s) => __kio_host_eprint__(s)".to_owned(),
        HostFnBodyKind::Exit { .. } => "(n) => __kio_host_exit__(n)".to_owned(),
        HostFnBodyKind::Loop => {
            "(step, s) => { while (true) { const out = step(s); \
             if (out !== null && typeof out === 'object') { \
             if (Object.prototype.hasOwnProperty.call(out, '_0')) { s = out._0; continue; } \
             if (Object.prototype.hasOwnProperty.call(out, '_1')) return out._1; } \
             if (Array.isArray(out)) { if (out[0] === 0) { s = out[1]; continue; } return out[1]; } \
             throw new Error('loop: step returned invalid sum'); } }"
                .to_owned()
        }
        HostFnBodyKind::ReadAsciiLine { .. } => {
            "() => { const line = __kio_host_read_ascii_line__(); \
             if (line == null) return { _1: null }; return { _0: line }; }"
                .to_owned()
        }
        HostFnBodyKind::NumericToString { value, .. } => {
            render_numeric_to_string_value(value.fixture.role())
        }
        HostFnBodyKind::BoolToString { .. } => "(v) => String(v)".to_owned(),
        HostFnBodyKind::PrintI32 { .. } => "(n) => __kio_host_print__(String(n))".to_owned(),
        HostFnBodyKind::StringToInt { .. } => {
            "(s) => { if (!/^[+-]?\\d+$/.test(s)) return { _1: null }; \
             const n = BigInt(s); \
             if (n < -2147483648n || n > 2147483647n) return { _1: null }; \
             return { _0: Number(n) }; }"
                .to_owned()
        }
        HostFnBodyKind::StringConcat { .. } => "(a, b) => a + b".to_owned(),
        HostFnBodyKind::StringEq { .. } => "(a, b) => a === b".to_owned(),
        HostFnBodyKind::StringLen { .. } => "(s) => s.length".to_owned(),
        HostFnBodyKind::StringSlice { .. } => {
            "(s, start, end) => { \
             if (start < 0 || start > end || end > s.length) \
             throw new Error('string_slice: invalid range [' + start + ', ' + end + ') for len ' + s.length); \
             return s.slice(start, end); }"
                .to_owned()
        }
        HostFnBodyKind::StringCodeAt { .. } => {
            "(s, index) => { if (index < 0 || index >= s.length) return { _1: null }; \
             return { _0: s.charCodeAt(index) }; }"
                .to_owned()
        }
        HostFnBodyKind::Array { operation, .. } => render_array_value(operation),
        // `dyn_load_prime`'s opaque-scalar surface. A scalar is a tagged `{k, v}`
        // object the host owns; the interpreter never reads it. `make_scalar`
        // maps fixture-private representation keys to shapes; `scalar_as_*`
        // returns the `. | <Kind>` sum (`{_1: v}` present, `{_0: null}`
        // absent — `()` is the left arm).
        HostFnBodyKind::MakeScalar { .. } => {
            "(text, representation) => { if (representation === 'I32' || representation === 'Int') return { k: 'i32', v: Number(BigInt.asIntN(32, BigInt(text))) }; \
             if (representation === 'F64' || representation === 'F32') return { k: 'f64', v: Number(text) }; \
             if (representation === 'String' || representation === 'Str') return { k: 'str', v: text }; \
             if (representation === 'Bool') return { k: 'bool', v: text === 't' }; \
             throw new Error('make_scalar: unknown representation key ' + representation); }"
                .to_owned()
        }
        HostFnBodyKind::ScalarOf { value, .. } => {
            format!(
                "(v) => ({{ k: {}, v }})",
                js_string_literal(value.fixture.role())
            )
        }
        HostFnBodyKind::ScalarAs { value, .. } => format!(
            "(s) => (s.k === {} ? {{ _1: s.v }} : {{ _0: null }})",
            js_string_literal(value.fixture.role())
        ),
        HostFnBodyKind::ScalarIsTrue { .. } => "(s) => s.v === true".to_owned(),
        HostFnBodyKind::Arithmetic { operation, number } => {
            render_numeric_arith_value(operation, number.fixture.role())
        }
        HostFnBodyKind::Compare {
            operation, number, ..
        } => {
            render_numeric_cmp_value(operation, number.fixture.role())
        }
        HostFnBodyKind::FloatArithmetic { operation, number } => {
            render_float_arith_value(operation, number.fixture.role())
        }
    }
}

fn render_array_value(operation: &str) -> String {
    match operation {
        "make-empty" => "() => []".to_owned(),
        "make-filled" => {
            "(n, fill) => { if (n < 0) throw new Error('array_make_filled: negative size ' + n); \
             const a = new Array(n); for (let i = 0; i < n; ++i) a[i] = fill; return a; }"
                .to_owned()
        }
        "len" => "(a) => a.length".to_owned(),
        "get" => "(a, i) => { if (i < 0 || i >= a.length) \
             throw new Error('array_get: index ' + i + ' out of bounds (len ' + a.length + ')'); \
             return a[i]; }"
            .to_owned(),
        "set" => "(a, i, v) => { if (i < 0 || i >= a.length) \
             throw new Error('array_set: index ' + i + ' out of bounds (len ' + a.length + ')'); \
             a[i] = v; return null; }"
            .to_owned(),
        "push" => "(a, v) => { a.push(v); return null; }".to_owned(),
        "pop-back" => "(a) => a.length === 0 ? { _1: null } : { _0: a.pop() }".to_owned(),
        "swap" => "(a, i, j) => { if (i < 0 || i >= a.length) \
             throw new Error('array_swap: index ' + i + ' out of bounds (len ' + a.length + ')'); \
             if (j < 0 || j >= a.length) \
             throw new Error('array_swap: index ' + j + ' out of bounds (len ' + a.length + ')'); \
             const t = a[i]; a[i] = a[j]; a[j] = t; return null; }"
            .to_owned(),
        "clear" => "(a) => { a.length = 0; return null; }".to_owned(),
        "clone" => "(a) => a.slice()".to_owned(),
        other => unreachable!("unknown protocol array operation `{other}`"),
    }
}

/// Whether the JS value shape for integer `kind` is `BigInt` rather
/// than `Number`: the wide (≥64-bit) integer roles per
/// `specs/backends/js.md` § Atomic types. The ≤32-bit roles are
/// JS `Number`; the wide roles are JS `BigInt`.
fn js_kind_is_bigint(kind: &str) -> bool {
    integer_bit_width(kind).is_some_and(|bits| bits >= 64)
}

fn render_numeric_to_string_value(kind: &str) -> String {
    assert!(
        integer_bit_width(kind).is_some() || FLOAT_KINDS.contains(&kind),
        "unknown protocol numeric role `{kind}`"
    );
    // `String` stringifies a JS `Number` and a JS `BigInt` alike
    // (`String(42)` / `String(42n)` both yield "42"), so the wide
    // (BigInt) and narrow (Number) roles share this body.
    "(n) => String(n)".to_owned()
}

fn render_numeric_arith_value(operation: &str, kind: &str) -> String {
    let js_op = match operation {
        "add" => "+",
        "sub" => "-",
        "mul" => "*",
        "div" => "/",
        "mod" => "%",
        _ => unreachable!("unknown protocol integer operation `{operation}`"),
    };
    let bits = integer_bit_width(kind).expect("integer kind has a width");
    let as_n = if is_signed_integer(kind) {
        "asIntN"
    } else if is_unsigned_integer(kind) {
        "asUintN"
    } else {
        unreachable!("a fixed-width integer kind is signed or unsigned");
    };
    // Operate in BigInt at the kind's width, then narrow back to the
    // role's JS value shape: a JS `Number` for the ≤32-bit roles, a JS
    // `BigInt` for the wide roles (`Number(2n ** 64n)` would lose
    // precision, so the wide roles stay BigInt — matching the literals
    // the JS backend emits with the `n` suffix).
    let wrapped = format!("BigInt.{as_n}({bits}, BigInt(a) {js_op} BigInt(b))");
    if js_kind_is_bigint(kind) {
        format!("(a, b) => {wrapped}")
    } else {
        format!("(a, b) => Number({wrapped})")
    }
}

fn render_numeric_cmp_value(operation: &str, kind: &str) -> String {
    integer_bit_width(kind).unwrap_or_else(|| panic!("unknown protocol integer role `{kind}`"));
    let js_op = match operation {
        "eq" => "===",
        "lt" => "<",
        "leq" | "le" => "<=",
        "gt" => ">",
        "geq" | "ge" => ">=",
        _ => unreachable!("unknown protocol comparison `{operation}`"),
    };
    // The comparison operators apply uniformly to JS `Number` and JS
    // `BigInt` (`1n === 1n`, `1n < 2n`), and both operands share the
    // kind, so a single body covers narrow and wide roles.
    format!("(a, b) => a {js_op} b")
}

/// The JS body for a float IEEE-754 arithmetic operation. Floats ride as
/// JS `Number`s, so the operation is the bare JS operator —
/// JS `Number` arithmetic *is* IEEE-754 double, matching Rust's `f64` (and
/// `f32` widened to `f64`).
fn render_float_arith_value(operation: &str, kind: &str) -> String {
    assert!(
        FLOAT_KINDS.contains(&kind),
        "unknown protocol float role `{kind}`"
    );
    let js_op = match operation {
        "add" => "+",
        "sub" => "-",
        "mul" => "*",
        "div" => "/",
        _ => unreachable!("unknown protocol float operation `{operation}`"),
    };
    format!("(a, b) => a {js_op} b")
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

/// Native implementation of `print(String) -> .`. Writes the argument
/// to stdout exactly (no implicit newline), then flushes so test goldens
/// can pin `expected.stdout` precisely.
///
/// Takes the JS argument as a generic `Value` so the body can shape
/// the conversion: when the value is a JS string it round-trips
/// through `to_string()`, otherwise its `String(v)` coercion (handled
/// by the surrounding arrow-fn) won't have applied — that case is
/// Kio code passing something other than a string into a
/// `String`-typed host parameter, which the surface type system
/// already excludes, so we treat a non-string as a no-op rather than
/// erroring loudly.
fn host_print(value: Value<'_>) {
    if let Some(s) = value.as_string()
        && let Ok(s) = s.to_string()
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}

/// Native implementation of `eprint(String) -> .`. The stderr mirror
/// of `print` — same write-and-flush, different stream.
fn host_eprint(value: Value<'_>) {
    if let Some(s) = value.as_string()
        && let Ok(s) = s.to_string()
    {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(s.as_bytes());
        let _ = err.flush();
    }
}

/// Native implementation of `exitInt -> !`. Exits the process with
/// the supplied code, clamped to 0..=125 — codes ≥ 126 collide with
/// shell and signal conventions per `specs/exit-codes.md`. Type-level
/// the function never returns, so any JS execution past this point is
/// unreachable in well-typed programs.
///
/// `process::exit` doesn't unwind, so abandoning the QuickJS call
/// stack underneath is safe: no Rust destructors run, no C state
/// needs cleaning.
fn host_exit(value: Value<'_>) {
    // Kio code passes a `Number`; try the obvious conversion paths.
    // BigInt is permitted as a fallback for the wider integer roles
    // — `exit(Int)` is i32-typed in current goldens, so this is
    // defensive rather than load-bearing.
    let code: i32 = if let Some(n) = value.as_int() {
        n
    } else if let Some(f) = value.as_float() {
        f as i32
    } else if let Some(b) = value.as_big_int() {
        // Convert BigInt to i64 via its to_i64 method; cast to i32.
        b.clone().to_i64().unwrap_or(0) as i32
    } else {
        0
    };
    process::exit(code.clamp(0, 125));
}

/// Native implementation of `read_ascii_line() -> String | .`. Reads one
/// stdin line at call time so the runner's standard streams behave
/// like ordinary process streams under harness redirection.
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

/// The branded factory export for an independently supplied artifact
/// namespace: `greeter` → `Greeter` → `createGreeter`.
fn js_factory_name(namespace: &str) -> String {
    format!("create{}", pascal_case(namespace))
}

fn package_js_module(dir: &Path, namespace: &str) -> Result<PathBuf, String> {
    let path = dir.join(format!("{namespace}.js"));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "expected JS package module `{}` for artifact namespace `{namespace}`",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HostRoleRef, RoleFixture};

    fn role(fixture: RoleFixture) -> HostRoleRef {
        HostRoleRef::new("testapi", fixture.role(), fixture)
    }

    #[test]
    fn package_factory_result_must_be_an_object() {
        let runtime = Runtime::new().expect("runtime");
        let context = Context::full(&runtime).expect("context");
        context.with(|ctx| {
            let value: Value<'_> = ctx.eval("42").expect("number");
            assert_eq!(
                require_package_object(value, "createDemo").expect_err("number is not a package"),
                "createDemo(host) returned a non-object"
            );

            let value: Value<'_> = ctx.eval("({})").expect("object");
            assert!(require_package_object(value, "createDemo").is_ok());
        });
    }

    #[test]
    fn main_lookup_uses_only_the_exact_declaring_module() {
        let runtime = Runtime::new().expect("runtime");
        let context = Context::full(&runtime).expect("context");
        context.with(|ctx| {
            let pkg: Object<'_> = ctx
                .eval(
                    "({ main: { main() {} }, testapi: { KioModule_main: { main() {} } }, prog: { main() {} } })",
                )
                .expect("package object");

            assert!(main_callable(&ctx, &pkg, "main").is_ok());
            assert!(main_callable(&ctx, &pkg, "testapi/main").is_ok());
            assert!(main_callable(&ctx, &pkg, "prog").is_ok());
            assert_eq!(
                main_callable(&ctx, &pkg, "api")
                    .expect_err("api is absent"),
                "main module `api` is missing or not an object"
            );

            let non_callable: Object<'_> =
                ctx.eval("({ api: { main: 1 } })").expect("package object");
            assert_eq!(
                main_callable(&ctx, &non_callable, "api")
                    .expect_err("main is not callable"),
                "main export `api/main` is missing or not callable"
            );
        });
    }

    #[test]
    fn export_navigation_uses_role_framed_facade_selectors() {
        assert_eq!(
            export_base_js_expr(RunnerProtocol::ExportNamespaceRoundtrip, "main"),
            r#"globalThis.__kio_protocol_pkg__["testapi"]["KioModule_main"]"#
        );
        let rendered = render_newtype_visibility_facade_js("types", "left", "right");
        assert!(rendered.contains(r#"types["KioType_ConstructorOnly"]"#));

        let rendered = render_export_multilabel_roundtrip_js("main");
        assert!(rendered.contains(r#"main_ns["KioType_A"].get"#));
        assert!(!rendered.contains("main_ns.A"));
    }

    fn rendered_body(leaf: &'static str, body: HostFnBodyKind) -> String {
        render_js_host_fn_value(&HostFnBinding {
            module: "test/module",
            leaf,
            body,
        })
    }

    #[test]
    fn integer_widths_match_role_names() {
        assert_eq!(integer_bit_width("i8"), Some(8));
        assert_eq!(integer_bit_width("u16"), Some(16));
        assert_eq!(integer_bit_width("i32"), Some(32));
        assert_eq!(integer_bit_width("u64"), Some(64));
        assert_eq!(integer_bit_width("i128"), Some(128));
        assert_eq!(integer_bit_width("f32"), None);
        assert_eq!(integer_bit_width("f64"), None);
        assert_eq!(integer_bit_width("bool"), None);
        assert_eq!(integer_bit_width("str"), None);
    }

    #[test]
    fn signedness_predicates_partition_numeric_kinds() {
        const NUMERIC_KINDS: &[&str] = &[
            "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "f32", "f64",
        ];
        for kind in NUMERIC_KINDS {
            let signed = is_signed_integer(kind);
            let unsigned = is_unsigned_integer(kind);
            let float = FLOAT_KINDS.contains(kind);
            assert_eq!(
                signed as u8 + unsigned as u8 + float as u8,
                1,
                "kind {kind} should belong to exactly one numeric family"
            );
        }
    }

    /// Create a unique scratch dir under the OS temp root and a
    /// guard that removes it on drop. Used by the JS-module discovery
    /// tests below so each test gets a clean dir without depending
    /// on `tempfile`.
    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(label: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "kio-test-runner-{label}-{}-{}",
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

    /// Exact artifact addressing fails when the expected namespace is
    /// absent, even if the directory exists.
    #[test]
    fn package_js_module_errors_when_expected_module_is_absent() {
        let dir = ScratchDir::new("no-package-module");
        let err = package_js_module(dir.path(), "pkg").expect_err("expected error");
        assert!(err.contains("pkg.js"), "unexpected error text: {err}");
    }

    /// The harness-supplied namespace addresses the package module
    /// directly.
    #[test]
    fn package_js_module_uses_exact_namespace() {
        let dir = ScratchDir::new("one-package-module");
        let only = dir.path().join("pkg.js");
        fs::write(&only, "// stub").expect("write stub");
        let found = package_js_module(dir.path(), "pkg").expect("found");
        assert_eq!(found, only);
    }

    /// Unrelated files do not participate in identity selection.
    #[test]
    fn package_js_module_ignores_unrelated_modules() {
        let dir = ScratchDir::new("multi-package-module");
        let expected = dir.path().join("a.js");
        fs::write(&expected, "// stub").expect("write a");
        fs::write(dir.path().join("b.js"), "// stub").expect("write b");
        assert_eq!(
            package_js_module(dir.path(), "a").expect("addressed module"),
            expected
        );
    }

    #[test]
    fn runtime_rejects_a_non_object_factory_result() {
        let dir = ScratchDir::new("non-object-factory");
        fs::write(
            dir.path().join("pkg.js"),
            "export function createPkg(_host) { return 42; }\n",
        )
        .expect("write package");
        let protocol = RunnerProtocol::Empty;
        let runner = JsRunner::new(
            protocol,
            ArtifactIdentity {
                namespace: "pkg".to_owned(),
            },
        );
        let err = runner
            .execute_artifact(dir.path(), &runner.host_api(), protocol)
            .expect_err("non-object package must fail");
        assert_eq!(err, "createPkg(host) returned a non-object");
    }

    #[test]
    fn runtime_requires_the_protocols_exact_main_module() {
        let dir = ScratchDir::new("exact-main-module");
        fs::write(
            dir.path().join("pkg.js"),
            "export function createPkg(_host) { return { main: { main() {} } }; }\n",
        )
        .expect("write package");
        let protocol = RunnerProtocol::EmptyApiMain;
        let runner = JsRunner::new(
            protocol,
            ArtifactIdentity {
                namespace: "pkg".to_owned(),
            },
        );
        let err = runner
            .execute_artifact(dir.path(), &runner.host_api(), protocol)
            .expect_err("wrong module main must fail");
        assert_eq!(err, "main module `api` is missing or not an object");
    }

    /// The factory export name is `create<Handle>` for the PascalCase of
    /// the independently supplied namespace.
    #[test]
    fn js_factory_name_brands_the_namespace() {
        assert_eq!(js_factory_name("greeter"), "createGreeter");
        assert_eq!(js_factory_name("csv_reconcile"), "createCsvReconcile");
    }

    /// The runner's PascalCase agrees with the emitter's derivation, so
    /// the reconstructed factory name matches the emitted export.
    #[test]
    fn pascal_case_matches_the_emitter_derivation() {
        assert_eq!(pascal_case("greeter"), "Greeter");
        assert_eq!(pascal_case("csv_reconcile"), "CsvReconcile");
        assert_eq!(pascal_case("two-part"), "KioNs_74776f2d70617274");
    }

    /// A testapi protocol nests each host fn under the exact declaring
    /// module in its binding (`__host__.<NS>.<leaf>`), matching the
    /// emitted package's lookup. `testapi/io.print` → `TESTAPI_IO.print`.
    #[test]
    fn testapi_print_protocol_nests_under_declaring_module() {
        let protocol = RunnerProtocol::TestApiPrint;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("\"testapi_io\": ({"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
    }

    /// An exact testapi contract spanning several modules nests each
    /// host fn under its binding's own module namespace.
    #[test]
    fn testapi_bare_collection_nests_per_declaring_module() {
        let protocol = RunnerProtocol::TestApiBareCollection;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("\"testapi_io\": ({"));
        assert!(s.contains("\"testapi_fmt\": ({"));
        assert!(s.contains("\"testapi_text\": ({"));
        assert!(s.contains("\"testapi_arith\": ({"));
        assert!(s.contains("\"testapi_iter\": ({"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
        assert!(s.contains("\"add\": (a, b) => Number(BigInt.asIntN(32"));
    }

    /// The structured body contract, not the leaf spelling, selects the
    /// implementation. Numeric helpers over any fixed-width integer kind get
    /// real bodies.
    #[test]
    fn structured_numeric_bodies_ignore_misleading_leaf_spellings() {
        assert_eq!(
            rendered_body(
                "not_a_numeric_name",
                HostFnBodyKind::NumericToString {
                    value: role(RoleFixture::I32),
                    string: role(RoleFixture::String),
                }
            ),
            "(n) => String(n)"
        );
        assert!(
            rendered_body(
                "also_not_numeric",
                HostFnBodyKind::Arithmetic {
                    operation: "add",
                    number: role(RoleFixture::I32),
                }
            )
            .contains("(a, b) => Number(BigInt.asIntN(32")
        );
        assert_eq!(
            rendered_body(
                "misleading",
                HostFnBodyKind::Compare {
                    operation: "leq",
                    number: role(RoleFixture::I32),
                    bool_: role(RoleFixture::Bool),
                }
            ),
            "(a, b) => a <= b"
        );
        // `i64_to_string` is now canonical (BigInt stringifies the same
        // way as Number through `String`).
        assert_eq!(
            rendered_body(
                "anything",
                HostFnBodyKind::NumericToString {
                    value: role(RoleFixture::I64),
                    string: role(RoleFixture::String),
                }
            ),
            "(n) => String(n)"
        );
    }

    /// Wide-integer (≥64-bit) arithmetic stays in JS `BigInt` — the
    /// body wraps at the kind's width through `BigInt.asIntN` /
    /// `asUintN` and returns the BigInt directly (no `Number(...)`
    /// narrowing, which would lose precision past 2^53). The narrow
    /// (≤32-bit) roles narrow back to `Number`.
    #[test]
    fn wide_int_arith_bodies_stay_bigint() {
        assert!(
            rendered_body(
                "arbitrary",
                HostFnBodyKind::Arithmetic {
                    operation: "add",
                    number: role(RoleFixture::I64),
                }
            )
            .contains("(a, b) => BigInt.asIntN(64, BigInt(a) + BigInt(b))")
        );
        assert!(
            rendered_body(
                "arbitrary",
                HostFnBodyKind::Arithmetic {
                    operation: "mul",
                    number: role(RoleFixture::U64),
                }
            )
            .contains("(a, b) => BigInt.asUintN(64, BigInt(a) * BigInt(b))")
        );
        assert!(
            rendered_body(
                "arbitrary",
                HostFnBodyKind::Arithmetic {
                    operation: "add",
                    number: role(RoleFixture::I128),
                }
            )
            .contains("(a, b) => BigInt.asIntN(128, BigInt(a) + BigInt(b))")
        );
        // The narrow i32 role still narrows back to a JS Number.
        assert!(
            rendered_body(
                "arbitrary",
                HostFnBodyKind::Arithmetic {
                    operation: "add",
                    number: role(RoleFixture::I32),
                }
            )
            .contains("(a, b) => Number(BigInt.asIntN(32")
        );
    }

    /// The exact loop protocol's env includes `loop`; its canonical body
    /// consumes the host-facing sum shape `{_0}` / `{_1}`.
    #[test]
    fn compute_loop_protocol_consumes_sum_shape() {
        let protocol = RunnerProtocol::TestApiComputeLoop;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("Object.prototype.hasOwnProperty.call(out, '_0')"));
        assert!(s.contains("Object.prototype.hasOwnProperty.call(out, '_1')"));
        assert!(s.contains("Array.isArray(out)"));
    }

    #[test]
    fn host_callback_protocol_installs_protocol_fns() {
        let protocol = RunnerProtocol::HostCallbackRoundtrip;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains(
            "\"callStep\": (step, seed) => step({ _0: seed, _1: 'compound-callback', _2: true })"
        ));
        assert!(s.contains("\"makePair\": (build, seed) => build(seed)._0"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
    }

    #[test]
    fn host_callback_return_protocol_installs_protocol_fn() {
        let protocol = RunnerProtocol::HostCallbackReturnRoundtrip;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("\"makeStep\": (delta) => (n) => Number(BigInt.asIntN(32"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
    }

    #[test]
    fn host_generic_type_protocol_installs_protocol_fns() {
        let protocol = RunnerProtocol::HostGenericTypeRoundtrip;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("\"boxMake\": (v) => ({ value: v })"));
        assert!(s.contains("\"boxGet\": (box) => box.value"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
    }

    #[test]
    fn host_structural_protocol_installs_protocol_fns() {
        let protocol = RunnerProtocol::HostStructuralRoundtrip;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("\"makePair\": (n, s) => ({ _0: n, _1: s })"));
        assert!(s.contains("\"sumToString\": (v) => ('_0' in v ? String(v._0) : v._1)"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
    }

    /// The testapi `host-type-roundtrip` protocol nests its bespoke
    /// `make_token` / `token_value` under `TESTAPI_OPAQUE`, `print`
    /// under `TESTAPI_IO`, matching the conformed golden's emitted
    /// `__host__.<NS>.<leaf>` lookup.
    #[test]
    fn host_type_protocol_installs_opaque_type_fns() {
        let protocol = RunnerProtocol::HostTypeRoundtrip;
        let host = host_api::dynamic_host_api(protocol.contract());
        let s = build_host_record_expression(&host, protocol);
        assert!(s.contains("\"testapi_opaque\": ({"));
        assert!(s.contains("\"makeToken\": (n) => ({ value: n })"));
        assert!(s.contains("\"tokenValue\": (token) => token.value"));
        assert!(s.contains("\"testapi_io\": ({"));
        assert!(s.contains("\"print\": (s) => __kio_host_print__(s)"));
    }
}

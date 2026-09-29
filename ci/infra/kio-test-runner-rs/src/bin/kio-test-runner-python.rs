//! `kio-test-runner-python` — imports the Python module emitted by
//! `kio build python`, synthesizes the selected protocol host object, and
//! runs the protocol's fixed export driver.
//!
//! The corpus harness supplies the package's artifact namespace
//! independently of emitted source. The runner uses it to address
//! `<namespace>.py` and the `create_<value-brand>` factory per
//! `specs/backends/python.md` § Output layout; it does not scan the
//! output directory or inspect package source (`.kio` / `.pkg.kio`).
//! The host API it drives still comes entirely from the **protocol**
//! (`--protocol <name>`): one complete named host and execution contract,
//! rendered in Python.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};

#[path = "../shared/artifact_identity.rs"]
mod artifact_identity;
#[path = "../shared/host_api.rs"]
#[allow(dead_code)]
mod host_api;
#[path = "../shared/protocol.rs"]
#[allow(dead_code)]
mod protocol;
#[path = "../shared/runner.rs"]
mod runner;

use artifact_identity::{ArtifactIdentity, ArtifactIdentityArgs};
use host_api::{HostApi, facade_module_selector, facade_type_selector, host_module_key};
use protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, HostTypeBinding, HostTypeFixture,
    ProtocolExecution, RunnerProtocol, WIDE_CALLABLE_SLOT_COUNT,
};
use runner::{EXIT_RUNTIME_FAILURE, EXIT_USAGE, TestRunner};

const USAGE: &str = "\
Usage: kio-test-runner-python [--protocol <name>] <output-dir>

Import and run the Python module emitted by `kio build python`.

Arguments:
  <output-dir>   Directory containing the Python package module
                 (`<ns>.py`) emitted by `kio build python`.

Options:
  --package-name <name>
                 Kio source package name supplied by the corpus harness.
                 Repeat exactly twice for `--protocol coexist`.
  --artifact-namespace <namespace>
                 Effective namespace of the preceding package artifact.
  --protocol <name>
                 Host/Kio interaction protocol to run. Defaults to
                 `empty-main`, the exact empty-host contract. See the
                 runner README for the protocol catalogue.
  -h, --help      Show this help and exit.
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    let mut protocol = RunnerProtocol::default();
    let mut protocol_seen = false;
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
            "--protocol" => {
                if protocol_seen {
                    eprintln!("error: --protocol specified more than once");
                    return EXIT_USAGE;
                }
                let Some(name) = iter.next() else {
                    eprintln!("error: --protocol requires a value");
                    return EXIT_USAGE;
                };
                protocol = match RunnerProtocol::parse(name) {
                    Ok(protocol) => protocol,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
                protocol_seen = true;
            }
            s if s.starts_with("--protocol=") => {
                if protocol_seen {
                    eprintln!("error: --protocol specified more than once");
                    return EXIT_USAGE;
                }
                protocol = match RunnerProtocol::parse(&s["--protocol=".len()..]) {
                    Ok(protocol) => protocol,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return EXIT_USAGE;
                    }
                };
                protocol_seen = true;
            }
            s if s.starts_with("--") => {
                eprintln!("error: unknown option: {s}");
                return EXIT_USAGE;
            }
            s => positional.push(s),
        }
    }

    let expected_packages = if protocol == RunnerProtocol::Coexist {
        2
    } else {
        1
    };
    let identities = match identity_args.resolve("python", expected_packages) {
        Ok(identities) => identities,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one.
    if protocol == RunnerProtocol::Coexist {
        let (a, b) = match positional.as_slice() {
            [a, b] => (Path::new(*a), Path::new(*b)),
            _ => {
                eprintln!("error: --protocol coexist takes exactly two <output-dir> arguments");
                return EXIT_USAGE;
            }
        };
        return match run_coexist(a, &identities[0], b, &identities[1]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: executing coexist artifacts: {e}");
                EXIT_RUNTIME_FAILURE
            }
        };
    }

    let dir = match positional.as_slice() {
        [d] => Path::new(*d),
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };

    PythonRunner {
        protocol,
        identity: identities.into_iter().next().unwrap(),
    }
    .run(dir, protocol)
}

/// The `coexist` protocol's two-artifact execution: one Python process
/// imports both emitted modules and interleaves calls through
/// per-package prefixing hosts (`shared/protocol.rs` § The coexist
/// protocol).
fn run_coexist(
    dir_a: &Path,
    identity_a: &ArtifactIdentity,
    dir_b: &Path,
    identity_b: &ArtifactIdentity,
) -> Result<i32, String> {
    let module_a = package_py_module(dir_a, &identity_a.namespace)?;
    let module_b = package_py_module(dir_b, &identity_b.namespace)?;
    let (stem_a, stem_b) = (&identity_a.namespace, &identity_b.namespace);
    if stem_a == stem_b {
        return Err(format!(
            "coexist requires two distinct package namespaces; both artifacts are `{stem_a}`"
        ));
    }
    let script = format!(
        r#"import importlib.util
import sys
import types

def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

def _host(prefix):
    def _print(s):
        sys.stdout.write(prefix + s)
        return None
    host = types.SimpleNamespace(greeter=types.SimpleNamespace(print=_print))
    setattr(host, "KioHostIn_" + {string_frame}, lambda value: value)
    setattr(host, "KioHostOut_" + {string_frame}, lambda value: value)
    setattr(host, "KioHostIn_" + {i32_frame}, lambda value: value)
    setattr(host, "KioHostOut_" + {i32_frame}, lambda value: value)
    return host

def _pair(pkg, label):
    q = getattr(pkg.greeter, {main_selector}).pair()
    sys.stdout.write(label + " pair: " + str(q["_0"]) + " " + str(q["_1"]) + "\n")

_ma = _load({path_a}, {lit_a})
_mb = _load({path_b}, {lit_b})
_pa = getattr(_ma, {factory_a})(_host("first: "))
_pb = getattr(_mb, {factory_b})(_host("second: "))
getattr(_pa.greeter, {main_selector}).main()
getattr(_pb.greeter, {main_selector}).main()
getattr(_pa.greeter, {main_selector}).main()
_pair(_pa, "first")
_pair(_pb, "second")
_pair(_pa, "first")
"#,
        path_a = python_string_literal(&module_a.display().to_string()),
        path_b = python_string_literal(&module_b.display().to_string()),
        lit_a = python_string_literal(stem_a),
        lit_b = python_string_literal(stem_b),
        factory_a = python_string_literal(&format!(
            "create_{}",
            artifact_identity::value_brand(stem_a)
        )),
        factory_b = python_string_literal(&format!(
            "create_{}",
            artifact_identity::value_brand(stem_b)
        )),
        main_selector = python_string_literal(&facade_module_selector("main", false)),
        string_frame = python_string_literal(&python_role_adapter_identity("greeter", "String")),
        i32_frame = python_string_literal(&python_role_adapter_identity("greeter", "I32")),
    );
    let mut child = Command::new(python_command())
        .arg("-c")
        .arg(script)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawning {}: {e}", python_command()))?;
    let status = child
        .wait()
        .map_err(|e| format!("waiting for {}: {e}", python_command()))?;
    Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
}

struct PythonRunner {
    protocol: RunnerProtocol,
    identity: ArtifactIdentity,
}

impl TestRunner for PythonRunner {
    fn host_api(&self) -> HostApi {
        host_api::dynamic_host_api(self.protocol.contract())
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host_api: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        let package_module = package_py_module(output_dir, &self.identity.namespace)?;
        let script = build_driver_script_with_host(
            &package_module,
            &self.identity.namespace,
            host_api,
            protocol,
        );
        let mut child = Command::new(python_command())
            .arg("-c")
            .arg(script)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawning {}: {e}", python_command()))?;
        let status = child
            .wait()
            .map_err(|e| format!("waiting for {}: {e}", python_command()))?;
        Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
    }
}

#[cfg(test)]
fn build_driver_script(package_module: &Path, namespace: &str, protocol: RunnerProtocol) -> String {
    let host_api = host_api::dynamic_host_api(protocol.contract());
    build_driver_script_with_host(package_module, namespace, &host_api, protocol)
}

fn build_driver_script_with_host(
    package_module: &Path,
    namespace: &str,
    host_api: &HostApi,
    protocol: RunnerProtocol,
) -> String {
    let host_items = host_items_literal(host_api, protocol);
    if protocol == RunnerProtocol::RustCallbackAliases {
        return "raise RuntimeError('rust-callback-aliases tests the Rust public naming contract only')\n".to_owned();
    }
    let host_types = host_types_literal(protocol.contract().host_types);
    let driver = python_driver(protocol);
    // The package's public factory is `create_<value-brand>`, derived from
    // the independently supplied artifact identity.
    let factory = format!("create_{}", artifact_identity::value_brand(namespace));
    let export_root = match protocol.export_root() {
        Some(root) => python_string_literal(&root),
        None => "None".to_owned(),
    };
    let has_export_root = protocol.export_root().is_some();
    let module_selectors = [
        "main",
        "api",
        "utils",
        "types",
        "left",
        "right",
        "a",
        "b",
        "child",
        "foo",
        "bar",
        "foo_bar",
        "i",
        "host",
        "mod_api_value",
    ]
    .into_iter()
    .map(|source| {
        format!(
            "{}: {}",
            python_string_literal(source),
            python_string_literal(&facade_module_selector(source, !has_export_root))
        )
    })
    .collect::<Vec<_>>()
    .join(", ");
    let module_selectors = format!("{{{module_selectors}}}");
    let main_module_source = match protocol.main_module() {
        Some(module) => python_string_literal(module),
        None => "None".to_owned(),
    };
    let main_module_selectors = match protocol.main_module() {
        Some(module) => {
            let selectors = module
                .split('/')
                .enumerate()
                .map(|(index, source)| {
                    python_string_literal(&facade_module_selector(source, index == 0))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{selectors}]")
        }
        None => "None".to_owned(),
    };
    let type_selectors = [
        "A",
        "Pair",
        "Box",
        "Functor",
        "Constructor_only",
        "Projector_only",
        "Both_public",
        "Shared",
        "Constructor_pair",
        "Projector_pair",
        "Constructor_generic",
        "Projector_generic",
        "Packed_function",
        "Packed",
        "Existential_unit",
        "Existential_empty",
        "Recursive_both",
        "Recursive_constructor",
        "Recursive_projector",
        "Constructor_spread",
        "Projector_spread",
        "Existential_spread",
        "Recursive_existential_function",
        "Root",
        "Child",
    ]
    .into_iter()
    .map(|source| {
        format!(
            "{}: {}",
            python_string_literal(source),
            python_string_literal(&facade_type_selector(source))
        )
    })
    .collect::<Vec<_>>()
    .join(", ");
    let type_selectors = format!("{{{type_selectors}}}");
    format!(
        r#"
import importlib.util
import re
import sys
import types

MODULE_PATH = {module_path}
PROTOCOL = {protocol_name}
EXPORT_ROOT = {export_root}
MODULE_SELECTORS = {module_selectors}
MAIN_MODULE = {main_module_source}
MAIN_MODULE_SELECTORS = {main_module_selectors}
TYPE_SELECTORS = {type_selectors}
HOST_ITEMS = {host_items}
HOST_TYPES = {host_types}

def _load_package(path):
    spec = importlib.util.spec_from_file_location("kio_emitted_package", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

def _ensure_ns(root, ns):
    current = root
    for part in ns.split("."):
        child = getattr(current, part, None)
        if child is None:
            child = types.SimpleNamespace()
            setattr(current, part, child)
        current = child
    return current

def _set_host(ns, leaf, fn):
    if ns is None:
        setattr(HOST, leaf, fn)
    else:
        setattr(_ensure_ns(HOST, ns), leaf, fn)

def _print(s):
    sys.stdout.write(str(s))
    sys.stdout.flush()

def _eprint(s):
    sys.stderr.write(str(s))
    sys.stderr.flush()

def _wrap_int(value, bits, signed):
    mask = (1 << bits) - 1
    value = int(value) & mask
    if signed and value >= (1 << (bits - 1)):
        value -= 1 << bits
    return value

def _trunc_div(a, b):
    a = int(a)
    b = int(b)
    q = abs(a) // abs(b)
    return -q if (a < 0) != (b < 0) else q

def _trunc_mod(a, b):
    return int(a) - _trunc_div(a, b) * int(b)

def _int_kind(name):
    widths = {{
        "i8": (8, True), "i16": (16, True), "i32": (32, True),
        "i64": (64, True), "i128": (128, True),
        "u8": (8, False), "u16": (16, False), "u32": (32, False),
        "u64": (64, False), "u128": (128, False),
    }}
    return widths.get(name)

def _arith(op, kind, a, b):
    if kind in ("f32", "f64"):
        if op == "add": return a + b
        if op == "sub": return a - b
        if op == "mul": return a * b
        if op == "div": return a / b
    bits, signed = _int_kind(kind)
    if op == "add": value = int(a) + int(b)
    elif op == "sub": value = int(a) - int(b)
    elif op == "mul": value = int(a) * int(b)
    elif op == "div": value = _trunc_div(a, b)
    elif op == "mod": value = _trunc_mod(a, b)
    else: raise RuntimeError("unknown arithmetic op " + op)
    return _wrap_int(value, bits, signed)

def _cmp(op, a, b):
    if op == "eq": return a == b
    if op == "lt": return a < b
    if op in ("leq", "le"): return a <= b
    if op == "gt": return a > b
    if op in ("geq", "ge"): return a >= b
    raise RuntimeError("unknown comparison op " + op)

def _float_to_string(value):
    value = float(value)
    return str(int(value)) if value.is_integer() else str(value)

def _host_fn(tag, operation, kind, carrier):
    if tag == "observe-packed":
        return lambda value: observe_packed(value)
    if tag == "nested-curried-roundtrip":
        def nested_curried_roundtrip(callback):
            _print("round host probe: " + callback("host-left")("host-right") + "\n")
            return callback
        return nested_curried_roundtrip
    if tag == "invoke-substituted-unit-callback":
        return lambda callback: "host/" + callback(None)
    if tag == "returned-forall-unit":
        produced = False
        def returned_forall_unit():
            nonlocal produced
            if produced:
                _print("throw\n")
                raise RuntimeError("produce failed")
            produced = True
            _print("produce\n")
            return None
        return returned_forall_unit
    if tag == "trace-unit":
        return lambda: _print(operation + "\n")
    if tag == "staged-unit-call":
        return lambda _value: _print("staged Unit host call\n")
    if tag == "call-step":
        return lambda step, seed: step({{ "_0": seed, "_1": "compound-callback", "_2": True }})
    if tag == "make-pair-callback":
        return lambda build, seed: build(seed)["_0"]
    if tag == "make-step":
        return lambda delta: (lambda n: _wrap_int(int(n) + int(delta), 32, True))
    if tag == "box-make":
        return lambda v: _carrier_out(carrier, v)
    if tag == "box-get":
        return lambda box: _carrier_in(carrier, box)
    if tag == "apply-poly":
        return lambda f: f("rank-n\n")
    if tag == "make-pair-structural":
        return lambda n, s: {{ "_0": n, "_1": s }}
    if tag == "produce-pair":
        def produce_pair():
            _print("direct\n")
            return {{"_0": 7, "_1": "direct-value"}}
        return produce_pair
    if tag == "sum-to-string":
        return lambda v: str(v["_0"] if "_0" in v else v["_1"])
    if tag == "make-token":
        return lambda n: {{ "value": n }}
    if tag == "token-value":
        return lambda token: token["value"]
    if tag == "round-functor":
        return lambda d: d
    if tag == "round-picker":
        return lambda picker: picker
    if tag == "round-poly-thunk":
        return lambda thunk: thunk
    if tag == "round-poly-unit-slot":
        return lambda step: step
    if tag == "staged-second":
        return lambda _first, second: second
    if tag == "unreachable-i32-print":
        return lambda _n: (_ for _ in ()).throw(RuntimeError("unreachable host function"))
    if tag == "print":
        return lambda s: _print(s)
    if tag == "eprint":
        return lambda s: _eprint(s)
    if tag == "exit":
        return lambda n: sys.exit(max(0, min(125, int(n))))
    if tag == "read-ascii-line":
        def read_line():
            line = sys.stdin.readline()
            if line == "":
                return {{ "_1": None }}
            line = line.rstrip("\n").rstrip("\r")
            if not line.isascii():
                print("read_ascii_line: non-ASCII input", file=sys.stderr)
                sys.exit(1)
            return {{ "_0": line }}
        return read_line
    if tag == "loop":
        def loop(step, state):
            while True:
                out = step(state)
                if isinstance(out, dict):
                    if "_0" in out:
                        state = out["_0"]
                        continue
                    if "_1" in out:
                        return out["_1"]
                if isinstance(out, list):
                    if out[0] == 0:
                        state = out[1]
                        continue
                    return out[1]
                raise RuntimeError("loop: step returned invalid sum")
        return loop
    if tag == "numeric-to-string":
        if kind in ("f32", "f64"):
            return lambda n: _float_to_string(n)
        return lambda n: str(n)
    if tag == "bool-to-string":
        return lambda value: str(value).lower()
    if tag == "print-i32":
        return lambda n: _print(str(n))
    if tag == "string-to-int":
        def string_to_int(s):
            if not re.match(r"^[+-]?\d+$", s):
                return {{ "_1": None }}
            n = int(s)
            if n < -2147483648 or n > 2147483647:
                return {{ "_1": None }}
            return {{ "_0": n }}
        return string_to_int
    if tag == "string-concat":
        return lambda a, b: a + b
    if tag == "string-eq":
        return lambda a, b: a == b
    if tag == "string-len":
        return lambda s: len(s)
    if tag == "string-slice":
        def string_slice(s, start, end):
            if start < 0 or start > end or end > len(s):
                raise RuntimeError(f"string_slice: invalid range [{{start}}, {{end}}) for len {{len(s)}}")
            return s[start:end]
        return string_slice
    if tag == "string-code-at":
        return lambda s, index: {{ "_1": None }} if index < 0 or index >= len(s) else {{ "_0": ord(s[index]) }}
    if tag == "array":
        if operation == "make-empty":
            return lambda: _carrier_out(carrier, [])
        if operation == "make-filled":
            def array_make_filled(n, fill):
                if n < 0:
                    raise RuntimeError(f"array_make_filled: negative size {{n}}")
                return _carrier_out(carrier, [fill for _ in range(n)])
            return array_make_filled
        if operation == "len":
            return lambda a: len(_carrier_in(carrier, a))
        if operation == "get":
            def array_get(a, i):
                a = _carrier_in(carrier, a)
                if i < 0 or i >= len(a):
                    raise RuntimeError(f"array_get: index {{i}} out of bounds (len {{len(a)}})")
                return a[i]
            return array_get
        if operation == "set":
            def array_set(a, i, v):
                a = _carrier_in(carrier, a)
                if i < 0 or i >= len(a):
                    raise RuntimeError(f"array_set: index {{i}} out of bounds (len {{len(a)}})")
                a[i] = v
                return None
            return array_set
        if operation == "push":
            return lambda a, v: (_carrier_in(carrier, a).append(v), None)[1]
        if operation == "pop-back":
            def array_pop_back(a):
                a = _carrier_in(carrier, a)
                return {{ "_1": None }} if len(a) == 0 else {{ "_0": a.pop() }}
            return array_pop_back
        if operation == "swap":
            def array_swap(a, i, j):
                a = _carrier_in(carrier, a)
                if i < 0 or i >= len(a):
                    raise RuntimeError(f"array_swap: index {{i}} out of bounds (len {{len(a)}})")
                if j < 0 or j >= len(a):
                    raise RuntimeError(f"array_swap: index {{j}} out of bounds (len {{len(a)}})")
                a[i], a[j] = a[j], a[i]
                return None
            return array_swap
        if operation == "clear":
            return lambda a: (_carrier_in(carrier, a).clear(), None)[1]
        if operation == "clone":
            return lambda a: _carrier_out(carrier, list(_carrier_in(carrier, a)))
        raise RuntimeError("unknown array operation " + str(operation))
    if tag == "make-scalar":
        def make_scalar(text, representation):
            if representation in ("I32", "Int"): return {{ "k": "i32", "v": int(text) }}
            if representation in ("F64", "F32"): return {{ "k": "f64", "v": float(text) }}
            if representation in ("String", "Str"): return {{ "k": "str", "v": text }}
            if representation == "Bool": return {{ "k": "bool", "v": text == "t" }}
            raise RuntimeError("make_scalar: unknown representation key " + representation)
        return make_scalar
    if tag == "scalar-of":
        return lambda value: {{ "k": kind, "v": value }}
    if tag == "scalar-as":
        return lambda scalar: {{ "_1": scalar["v"] }} if scalar["k"] == kind else {{ "_0": None }}
    if tag == "scalar-is-true":
        return lambda scalar: scalar["v"] is True
    if tag in ("arithmetic", "float-arithmetic"):
        return lambda a, b: _arith(operation, kind, a, b)
    if tag == "compare":
        return lambda a, b: _cmp(operation, a, b)
    raise RuntimeError("kio-test-runner-python: unknown host-body tag " + tag)

module = _load_package(MODULE_PATH)
HOST = types.SimpleNamespace()
HOST_ROLE_FRAMES = {{}}
HOST_ROLE_ADAPTERS = {{}}
HOST_TYPE_FIXTURES = {{}}

def _selected_role_type(label):
    class SelectedRole:
        __slots__ = ("value",)
        def __init__(self, value):
            self.value = value
        def __repr__(self):
            return "<" + label + ">"
    SelectedRole.__name__ = "KioRunnerSelected_" + label
    return SelectedRole

def _role_adapter_identity(source_module, leaf, frame):
    def render(source):
        leading = len(source) - len(source.lstrip("_"))
        trailing = len(source) - len(source.rstrip("_"))
        words = source.strip("_").split("_")
        return "_" * leading + words[0] + "".join(word[:1].upper() + word[1:] for word in words[1:]) + "_" * trailing
    components = [render(source) for source in source_module.split("/") + [leaf]]
    if (not source_module or components[0] == "V1"
            or any(not component or not component.isascii()
                   or not component.isalnum() for component in components)):
        return frame
    return "_".join(components)

for source_module, leaf, frame, fixture in HOST_TYPES:
    HOST_TYPE_FIXTURES[frame] = fixture
    if fixture not in ("role", "selected-role"):
        continue
    adapter = _role_adapter_identity(source_module, leaf, frame)
    HOST_ROLE_FRAMES[(source_module, leaf)] = frame
    HOST_ROLE_ADAPTERS[frame] = adapter
    if fixture == "selected-role":
        selected = _selected_role_type(frame)
        def host_in(value, selected=selected, frame=frame):
            if type(value) is not selected:
                raise TypeError("expected exact host value " + frame)
            return value.value
        def host_out(value, selected=selected):
            return selected(value)
    else:
        host_in = lambda value: value
        host_out = lambda value: value
    setattr(HOST, "KioHostIn_" + adapter, host_in)
    setattr(HOST, "KioHostOut_" + adapter, host_out)

def _role_in(frame, value):
    return getattr(HOST, "KioHostIn_" + HOST_ROLE_ADAPTERS[frame])(value)

def _role_out(frame, value):
    return getattr(HOST, "KioHostOut_" + HOST_ROLE_ADAPTERS[frame])(value)

def _public_role(source_module, leaf, value):
    return _role_out(HOST_ROLE_FRAMES[(source_module, leaf)], value)

def _native_role(source_module, leaf, value):
    return _role_in(HOST_ROLE_FRAMES[(source_module, leaf)], value)

if PROTOCOL == "host-type-roundtrip":
    _role_probe = _public_role("testapi", "I32", 37)
    if _native_role("testapi", "I32", _role_probe) != 37:
        raise RuntimeError("readable role adapter did not round-trip")

def _carrier_in(frame, value):
    expected = getattr(module, "KioHostType_" + frame)[object]
    if type(value) is not expected:
        raise TypeError("expected exact host carrier " + frame)
    return value.to_native()

def _carrier_out(frame, value):
    return getattr(module, "KioHostType_" + frame)[object].from_native(value)

def _adapt_host_fn(fn, tag, roles):
    if not any(HOST_TYPE_FIXTURES[frame] == "selected-role" for frame in roles):
        return fn
    if tag in ("print", "eprint", "print-i32", "unreachable-i32-print", "exit"):
        return lambda value: fn(_role_in(roles[0], value))
    if tag in ("arithmetic", "float-arithmetic"):
        return lambda a, b: _role_out(
            roles[0], fn(_role_in(roles[0], a), _role_in(roles[0], b)))
    if tag == "numeric-to-string":
        return lambda value: _role_out(roles[1], fn(_role_in(roles[0], value)))
    if tag == "make-token":
        return lambda value: fn(_role_in(roles[0], value))
    if tag == "token-value":
        return lambda token: _role_out(roles[0], fn(token))
    raise RuntimeError("selected role has no Python runner body adapter: " + tag)

for ns, leaf, body_tag, operation, kind, roles, carrier in HOST_ITEMS:
    raw = _host_fn(body_tag, operation, kind, carrier)
    _set_host(ns, leaf, _adapt_host_fn(raw, body_tag, roles))

if PROTOCOL == "__compile_only__":
    sys.exit(0)
if PROTOCOL == "facade-selector-collisions":
    flat_host = types.SimpleNamespace(read=lambda _left, _right: -1)
    try:
        module.{factory}(flat_host)
    except RuntimeError as error:
        prefix = "missing host item: "
        message = str(error)
        if not message.startswith(prefix):
            raise
        missing = set(message[len(prefix):].split(", "))
        expected = {{
            (ns + "." + leaf) if ns is not None else leaf
            for ns, leaf, _tag, _operation, _kind, _roles, _carrier in HOST_ITEMS
        }}
        for _source_module, _leaf, frame, fixture in HOST_TYPES:
            if fixture in ("role", "selected-role"):
                adapter = _role_adapter_identity(_source_module, _leaf, frame)
                expected.add("KioHostIn_" + adapter)
                expected.add("KioHostOut_" + adapter)
        if missing != expected:
            raise RuntimeError("unexpected flat-host rejection: " + message)
    else:
        raise RuntimeError("flat host functions satisfied namespaced host items")
pkg = module.{factory}(HOST)

def _base(leaf):
    selector = MODULE_SELECTORS[leaf]
    if EXPORT_ROOT is None:
        return getattr(pkg, selector)
    return getattr(getattr(pkg, EXPORT_ROOT), selector)

def _root_base():
    if EXPORT_ROOT is None:
        return pkg
    return getattr(pkg, EXPORT_ROOT)

def _type(base, source):
    return getattr(base, TYPE_SELECTORS[source])

def _main():
    current = pkg
    for selector in MAIN_MODULE_SELECTORS:
        if not hasattr(current, selector):
            raise RuntimeError("missing main module `" + MAIN_MODULE + "`")
        current = getattr(current, selector)
    main = getattr(current, "main", None)
    if not callable(main):
        raise RuntimeError("main export `" + MAIN_MODULE + "/main` is missing or not callable")
    main()

if PROTOCOL in ("__construct_only__", "__compile_only__"):
    pass
elif PROTOCOL == "__main__":
    _main()
elif PROTOCOL == "export-namespace-roundtrip":
    main_ns = _base("main")
    utils_ns = _base("utils")
    _print(str(main_ns.answer()) + "\n")
    _print(str(utils_ns.echo("namespace-utils")) + "\n")
elif PROTOCOL == "export-callback-roundtrip":
    main = _base("main")
    _print(str(main.applyTwice(lambda n: n + 3, 10)) + "\n")
    _print(str(main.makeStep(4)(5)) + "\n")
elif PROTOCOL == "export-module-roundtrip":
    api = _base("api")
    _print(str(api.tag()) + "\n")
    _print(str(api.value()) + "\n")
    _print(str(api.echo("module-echo")) + "\n")
elif PROTOCOL == "export-multi-label-roundtrip":
    main = _base("main")
    main.say({{ "A": 42, "B": "shown\n" }})
    row = main.echoPair({{ "A": 88, "B": "99" }})
    a = getattr(main, TYPE_SELECTORS["A"])
    out_a = a.get(main.echoA(a.mk(111)))
    if type(row["A"]) is not int or type(row["B"]) is not str or type(out_a) is not int:
        raise RuntimeError("labels facade did not preserve the selected host representations")
    _print(str(row["A"]) + "\n")
    _print(row["B"] + "\n")
    _print(str(out_a) + "\n")
elif PROTOCOL == "export-poly-roundtrip":
    root = _root_base()
    _print(str(root.polyEcho("poly-string")) + "\n")
    _print(str(root.polyEcho(42)) + "\n")
    _print(str(root.keepLeft("left", 99)) + "\n")
elif PROTOCOL == "export-poly-callback-roundtrip":
    main = _base("main")
    _print(main.applyVia(lambda s: "via: " + s, "apply") + "\n")
    _print(str(main.applyVia(lambda n: n + 8, 7)) + "\n")
elif PROTOCOL == "export-callable-slots-roundtrip":
    main = _base("main")
    def check(actual, expected):
        if type(actual) is not type(expected) or actual != expected:
            raise RuntimeError("callable slot payload changed")
    calls = [0, 0]
    def product_step(value):
        calls[0] += 1
        return value + 5
    def sum_step(value):
        calls[1] += 1
        return value + 7
    check(main.applyProduct({{"_0": product_step, "_1": 11}}), 16)
    echoed_product = main.echoProduct({{"_0": product_step, "_1": 17}})
    check(echoed_product["_1"], 17)
    check(echoed_product["_0"](echoed_product["_1"]), 22)
    made_product = main.makeProduct(23)
    check(made_product["_1"], 23)
    check(made_product["_0"](29), 29)
    check(main.applySum({{"_0": sum_step}}, 31), 38)
    echoed_sum = main.echoSum({{"_0": sum_step}})
    check(list(echoed_sum), ["_0"])
    check(echoed_sum["_0"](37), 44)
    made_sum = main.makeCallableSum()
    check(list(made_sum), ["_0"])
    check(made_sum["_0"](41), 41)
    scalar = main.makeScalarSum(97)
    check(list(scalar), ["_1"])
    check(scalar["_1"], 97)
    check(main.applySum(scalar, 43), 97)
    echoed_scalar = main.echoSum(scalar)
    check(list(echoed_scalar), ["_1"])
    check(echoed_scalar["_1"], 97)
    check(calls, [2, 2])
    _print("callable slots ok\n")
elif PROTOCOL == "export-scalar-roundtrip":
    main = _base("main")
    samples = [
        (main.echoI128, -1208925819614629174706299),
        (main.echoU128, 2417851639229258349412391),
        (main.echoF32, 1.5), (main.echoF32, -2.25),
        (main.echoF64, 1.0000000000000002), (main.echoF64, -3.125),
    ]
    for echo, value in samples:
        returned = echo(value)
        if type(returned) is not type(value) or returned != value:
            raise RuntimeError("scalar payload changed")
    _print("scalar payloads ok\n")
elif PROTOCOL == "export-host-owned-roundtrip":
    main = _base("main")
    for value in (7, 19):
        returned = main.echoToken({{"value": value}})
        if type(returned["value"]) is not int or returned["value"] != value:
            raise RuntimeError("token payload changed")
    for value in (42, "box-value"):
        returned = _carrier_in({box_frame}, main.echoBox(_carrier_out({box_frame}, value)))
        if type(returned) is not type(value) or returned != value:
            raise RuntimeError("box payload changed")
    _print("host-owned payloads ok\n")
elif PROTOCOL == "export-structural-roundtrip":
    main = _base("main")
    q = main.pairSwap({{ "_0": 42, "_1": "hello" }})
    _print(str(q["_0"]) + " " + str(q["_1"]) + "\n")
    _print(str(main.dispatchLeft({{ "_0": 7 }})) + "\n")
    _print(str(main.dispatchLeft({{ "_1": "from-sum" }})) + "\n")
    wide = main.rotate({{ "_0": 1, "_1": 2, "_2": 3, "_3": 4, "_4": 5, "_5": 6, "_6": 7, "_7": 8, "_8": 9, "_9": 10, "_10": 11, "_11": 12 }})
    _print(str(wide["_0"]) + " " + str(wide["_1"]) + " " + str(wide["_11"]) + "\n")
    _print(str(main.classify({{ "_0": 1 }})) + "\n")
    _print(str(main.classify({{ "_4": 5 }})) + "\n")
    _print(str(main.classify({{ "_9": "ten" }})) + "\n")
    _print(str(main.classify(main.chooseFirst())) + "\n")
    _print(str(main.classify(main.chooseMiddle())) + "\n")
    _print(str(main.classify(main.chooseLast())) + "\n")
    samples = [
        (0, -101), (1, -12345), (2, -123456789), (3, -9007199254740993),
        (4, 201), (5, 54321), (6, 3456789012), (7, 18014398509481987),
        (8, False), (8, True), (9, "sum-value"),
    ]
    for arm, value in samples:
        key = "_" + str(arm)
        returned = main.echoSum({{key: value}})
        if list(returned) != [key] or type(returned[key]) is not type(value) or returned[key] != value:
            raise RuntimeError("export-structural-roundtrip: sum payload changed at arm " + str(arm))
        payload = str(returned[key]).lower() if type(value) is bool else str(returned[key])
        _print(str(main.classify(returned)) + " " + payload + "\n")
elif PROTOCOL == "export-positional-product-roundtrip":
    q = _base("main").makePair(7, "hello")
    _print(str(q["_0"]) + " " + str(q["_1"]) + "\n")
elif PROTOCOL == "host-existential-roundtrip":
    counts = [0, 0]
    def observe_packed(value):
        counts[0] += 1
        def open_payload(payload):
            counts[1] += 1
            return payload["_1"](payload["_0"])
        return _type(_base("types"), "Packed").readPacked(value)(open_payload)
    result = _base("main").exercise()
    if type(result["_0"]) is not int or type(result["_1"]) is not int or result["_0"] != 37 or result["_1"] != 83 or counts != [2, 2]:
        raise RuntimeError("existential host observations changed")
    _print("existential host opening ok\n")
elif PROTOCOL == "export-functor-dict-roundtrip":
    main = _base("main")
    box = _type(_base("types"), "Box")
    functor = _type(_base("types"), "Functor")
    integers, texts = [], []
    def to_text(value):
        integers.append(value)
        return "v:" + str(value)
    def to_integer(value):
        texts.append(value)
        return len(value)
    def wrap(value):
        return box.mkBox({{"_0": value, "_1": None}})
    def unwrap(value):
        row = box.unBox(value)
        if row["_1"] is not None:
            raise RuntimeError("functor box Unit changed")
        return row["_0"]
    def check(actual, expected):
        if type(actual) is not type(expected) or actual != expected:
            raise RuntimeError("functor payload changed")
    dictionary = main.echoFunctor(main.boxFunctor())
    check(unwrap(main.applyFunctor(dictionary, to_text, wrap(42))), "v:42")
    check(unwrap(main.applyFunctor(dictionary, to_integer, wrap("apple"))), 5)
    mapping = functor.fmap(dictionary)
    check(unwrap(mapping({{"_0": to_text, "_1": wrap(7)}})), "v:7")
    check(unwrap(mapping({{"_0": to_integer, "_1": wrap("pear")}})), 4)
    check(integers, [42, 7])
    check(texts, ["apple", "pear"])
    _print("functor dictionary ok\n")
elif PROTOCOL == "export-type-roundtrip":
    pair = _type(_base("types"), "Pair")
    payload = pair.unPair(pair.mkPair({{ "_0": "export-type-left", "_1": "export-type-right" }}))
    _print(str(payload["_0"]) + "\n")
    _print(str(payload["_1"]) + "\n")
elif PROTOCOL == "export-newtype-sum-roundtrip":
    main = _base("main")
    _print(str(main.firstOr(0, main.pack(7, "hi"))) + "\n")
elif PROTOCOL == "export-newtype-scalar-roundtrip":
    out = _base("main").bump({{ "Wrap": 7 }})
    _print(str(out["Wrap"]) + "\n")
elif PROTOCOL == "newtype-visibility-facade":
    facade = _base("types")
    left = _base("left")
    right = _base("right")
    i32 = lambda value: _public_role("testapi", "I32", value)
    native_i32 = lambda value: _native_role("testapi", "I32", value)
    a = facade.makeA(i32(11))
    b = facade.makeB(i32(22))
    c = _type(facade, "Constructor_only").makeConstructorOnly(i32(33))
    p = facade.makeProjectorOnlyValue(i32(44))
    d = _type(facade, "Both_public").makeBothPublic(i32(55))
    l = left.make(i32(66))
    r = right.make(i32(77))
    out_a = facade.readA(a)
    out_b = facade.readB(b)
    out_c = facade.readConstructorOnlyValue(c)
    out_p = _type(facade, "Projector_only").readProjectorOnly(p)
    out_d = _type(facade, "Both_public").readBothPublic(d)
    out_l = _type(left, "Shared").readShared(_type(left, "Shared").makeShared(left.read(l)))
    out_r = _type(right, "Shared").readShared(_type(right, "Shared").makeShared(right.read(r)))
    constructor_pair = _type(facade, "Constructor_pair").makeConstructorPair(
        {{ "_0": i32(81), "_1": i32(82) }})
    constructor_pair_out = facade.readConstructorPairValue(constructor_pair)
    projector_pair = facade.makeProjectorPairValue(constructor_pair_out)
    projector_pair_out = _type(facade, "Projector_pair").readProjectorPair(projector_pair)
    constructor_generic_out = facade.readConstructorGenericValue(
        _type(facade, "Constructor_generic").makeConstructorGeneric(i32(85)))
    projector_generic_out = _type(facade, "Projector_generic").readProjectorGeneric(
        facade.makeProjectorGenericValue(i32(86)))
    packed = _type(facade, "Packed_function").makePackedFunction(
        lambda value: i32(native_i32(value["_0"]) + native_i32(value["_1"])))
    unpacked = _type(facade, "Packed_function").readPackedFunction(packed)
    packed_out = unpacked(constructor_pair_out)
    existential_out = _type(facade, "Existential_unit").readExistentialUnit(
        facade.makeExistentialUnitValue())(lambda _value: i32(89))
    existential_empty_out = _type(facade, "Existential_empty").readExistentialEmpty(
        facade.makeExistentialEmptyValue())(lambda: i32(90))
    recursive_both = _type(facade, "Recursive_both").makeRecursiveBoth(
        facade.recursiveBothBasePayload())
    recursive_both_out = facade.recursiveBothPayloadIsBase(
        _type(facade, "Recursive_both").readRecursiveBoth(recursive_both))
    recursive_constructor = _type(facade, "Recursive_constructor").makeRecursiveConstructor(
        facade.recursiveConstructorBasePayload())
    recursive_constructor_out = facade.recursiveConstructorPayloadIsBase(
        facade.readRecursiveConstructorValue(recursive_constructor))
    recursive_projector_out = facade.recursiveProjectorPayloadIsBase(
        _type(facade, "Recursive_projector").readRecursiveProjector(
            facade.makeRecursiveProjectorBase()))
    constructor_inputs = []
    def constructor_step(value):
        seed = native_i32(value["_0"])
        constructor_inputs.append(seed)
        return i32(seed + 3)
    constructed = _type(facade, "Constructor_spread").makeConstructorSpread(constructor_step)
    if native_i32(facade.invokeConstructorSpreadI32(constructed, i32(101))) != 104 or native_i32(facade.invokeConstructorSpreadUnit(constructed, i32(102))) != 105 or constructor_inputs != [101, 102]:
        raise RuntimeError("constructor-only polymorphic callback changed")
    projected = _type(facade, "Projector_spread").readProjectorSpread(facade.makeProjectorSpreadValue())
    if native_i32(projected({{"_0": i32(111), "_1": i32(1)}})) != 111 or native_i32(projected({{"_0": i32(112), "_1": None}})) != 112:
        raise RuntimeError("projector-only polymorphic callback changed")
    spread_payloads = []
    recursive_payloads = []
    def open_spread(value):
        if not callable(value):
            raise RuntimeError("existential payload is not callable")
        spread_payloads.append(value)
        return i32(91)
    def open_recursive(value):
        if not callable(value):
            raise RuntimeError("existential payload is not callable")
        recursive_payloads.append(value)
        return i32(92)
    opened_spread = _type(facade, "Existential_spread").readExistentialSpread(facade.makeExistentialSpreadValue())(open_spread)
    opened_recursive = _type(facade, "Recursive_existential_function").readRecursiveExistentialFunction(facade.makeRecursiveExistentialFunctionValue())(open_recursive)
    if native_i32(opened_spread) != 91 or native_i32(opened_recursive) != 92 or len(spread_payloads) != 1 or len(recursive_payloads) != 1:
        raise RuntimeError("existential continuation count changed")
    cycle_outer = facade.makeVisibleCycleOuterBase()
    if type(cycle_outer) is not dict or "VisibleCycleOuter" not in cycle_outer:
        raise RuntimeError("hidden carrier did not cut the visible outer recursion")
    comptime_outer = facade.makeVisibleComptimeOuterBase()
    if type(comptime_outer) is not dict or "VisibleComptimeOuter" not in comptime_outer:
        raise RuntimeError("hidden compile-time payload changed the visible outer boundary")
    scalar_outputs = [
        ("a", out_a), ("b", out_b), ("constructor-only", out_c),
        ("projector-only", out_p), ("both-public", out_d),
        ("left-shared", out_l), ("right-shared", out_r),
        ("constructor-generic", constructor_generic_out),
        ("projector-generic", projector_generic_out),
        ("packed-function", packed_out),
        ("existential-unit", existential_out),
        ("existential-empty", existential_empty_out),
        ("recursive-both", recursive_both_out),
        ("recursive-constructor", recursive_constructor_out),
        ("recursive-projector", recursive_projector_out),
    ]
    for label, value in scalar_outputs:
        try:
            native = native_i32(value)
        except TypeError as error:
            raise RuntimeError(
                "facade did not preserve the selected testapi.I32 representation for "
                + label
                + ": "
                + repr(value)
            ) from error
        if type(native) is not int:
            raise RuntimeError(
                "facade did not preserve the selected testapi.I32 representation for " + label
            )
    pair_outputs = [constructor_pair_out, projector_pair_out]
    if any(
        type(value) is not dict
        or type(native_i32(value.get("_0"))) is not int
        or type(native_i32(value.get("_1"))) is not int
        for value in pair_outputs
    ):
        raise RuntimeError("facade did not preserve the compound product representation")
    _print(str(native_i32(out_a)) + "\n")
    _print(str(native_i32(out_b)) + "\n")
    _print(str(native_i32(out_c)) + "\n")
    _print(str(native_i32(out_p)) + "\n")
    _print(str(native_i32(out_d)) + "\n")
    _print(str(native_i32(out_l)) + "\n")
    _print(str(native_i32(out_r)) + "\n")
    _print(str(native_i32(constructor_pair_out["_0"])) + " " + str(native_i32(constructor_pair_out["_1"])) + "\n")
    _print(str(native_i32(projector_pair_out["_0"])) + " " + str(native_i32(projector_pair_out["_1"])) + "\n")
    _print(str(native_i32(constructor_generic_out)) + "\n")
    _print(str(native_i32(projector_generic_out)) + "\n")
    _print(str(native_i32(packed_out)) + "\n")
    _print(str(native_i32(existential_out)) + "\n")
    _print(str(native_i32(existential_empty_out)) + "\n")
    _print(str(native_i32(recursive_both_out)) + "\n")
    _print(str(native_i32(recursive_constructor_out)) + "\n")
    _print(str(native_i32(recursive_projector_out)) + "\n")
elif PROTOCOL == "export-nested-product-roundtrip":
    out = _base("main").make("nest", 7, 9)
    _print(str(out["Outer"]["_0"]) + "\n")
    _print(str(out["Outer"]["Inner"]["_0"]) + "\n")
    _print(str(out["Outer"]["Inner"]["_1"]) + "\n")
elif PROTOCOL == "public-word-names":
    word_api = _root_base().wordApi
    _print(str(word_api.readWord()) + "\n")
    _print(str(word_api._readWord()) + "\n")
    _print(str(word_api.readWord_()) + "\n")
    _print(str(word_api._readWord_()) + "\n")
    _print(str(word_api.readWord__()) + "\n")
    word_nodes = word_api.KioModule_wordNodes
    boxed = word_nodes.KioType__uWordBox_u_u.wrapWord(55)
    if boxed != {{"_WordBox__": 55}}:
        raise RuntimeError("incorrect public transparent wrapper key")
    _print(str(word_nodes.KioType__uWordBox_u_u.unwrapWord({{"_WordBox__": 55}})) + "\n")
    _print(str(word_nodes.keepWord(66)) + "\n")
    pair = word_api.keepPair({{"_WordBox__": 77, "wordApi/otherNodes._WordBox__": 88}})
    _print(str(pair["_WordBox__"]) + "\n")
    _print(str(pair["wordApi/otherNodes._WordBox__"]) + "\n")
elif PROTOCOL == "export-compound-input-once":
    api = _base("main")
    def line(value):
        _print(str(value) + "\n")
    direct = api.direct()
    line(direct["_0"])
    line(direct["_1"])
    def callback():
        line("callback")
        return {{"_0": 9, "_1": "callback-value"}}
    returned = api.callback(callback)
    line(returned["_0"])
    line(returned["_1"])
    reads = []
    class Tracked(dict):
        def __getitem__(self, key):
            reads.append(key)
            return super().__getitem__(key)
    nested_input = {{"Outer": Tracked({{"_0": "nest", "Inner": Tracked({{"_0": 11, "_1": 13}})}})}}
    nested = api.echoOuter(nested_input)["Outer"]
    if reads != ["_0", "Inner", "_0", "_1"]:
        raise RuntimeError("compound-input-once: product access order " + repr(reads))
    line(nested["_0"])
    line(nested["Inner"]["_0"])
    line(nested["Inner"]["_1"])
    for choice in [api.first(17), api.middle(19, 23), api.last("choice", 29, 31)]:
        payload = choice["Choice"]
        key, = payload
        reads.clear()
        returned = api.echoChoice({{"Choice": Tracked(payload)}})["Choice"]
        if reads != [key]:
            raise RuntimeError("compound-input-once: selected access " + repr(reads))
        value = returned[key]
        if key == "_0":
            line(value)
        elif key == "Inner":
            line(value["_0"])
            line(value["_1"])
        else:
            line(value["_0"])
            line(value["Inner"]["_0"])
            line(value["Inner"]["_1"])
    line(api.echoText("atomic"))
elif PROTOCOL == "export-curried-facade":
    _print(_base("main").pick("ku", "rz") + "\n")
    _print(str(_base("main").last(1, 2, 3)) + "\n")
elif PROTOCOL == "nested-curried-roundtrip":
    api = _base("api")
    join = lambda left: lambda right: left + "/" + right
    via_host = api.viaHost(join)
    _print("via host: " + via_host("env-left")("env-right") + "\n")
    round_export = api.roundExport(join)
    _print("round export: " + round_export("export-left")("export-right") + "\n")
elif PROTOCOL == "host-substituted-unit-callback":
    _print(_base("api").viaHost(lambda _unit: "callback") + "\n")
elif PROTOCOL == "returned-forall-call-by-value":
    run = _base("main").main
    run()
    try:
        run()
    except RuntimeError as error:
        if str(error) != "produce failed":
            raise
        _print("caught\n")
    else:
        raise RuntimeError("produce did not fail")
elif PROTOCOL == "export-newtype-ignored-argument-roundtrip":
    main = _base("main")
    wrapped = main.fromI32(7)
    if wrapped != {{ "Wrap": {{ "Const": 7 }} }}:
        raise RuntimeError("ignored type argument changed the public newtype carrier")
    _print(str(main.toI32(wrapped)) + "\n")
elif PROTOCOL == "recursive-newtype-boundary":
    main = _base("main")
    root_type = _type(main, "Root")
    payload = main.basePayload()
    root = root_type.makeRoot(payload)
    kept = main.keep(root)
    projected = root_type.readRoot(kept)
    _print(str(main.acceptPayload(projected)) + "\n")
elif PROTOCOL == "facade-selector-collisions":
    api = _base("api")
    api.pkg()
    api.value()
    _base("host").value()
    _base("mod_api_value").value()
    foo = _base("foo")
    _print(str(getattr(foo, MODULE_SELECTORS["bar"]).value(9, 1)) + "\n")
    _print(str(_base("foo_bar").value(10, 2)) + "\n")
    _print(str(_base("i").value(41, 1)) + "\n")
    api.child()
    _print("api.child function\n")
    getattr(api, MODULE_SELECTORS["child"]).value()
    _print("api/child module\n")
    child_type = _type(api, "Child")
    child = child_type.makeChild(30)
    child_type.readChild(child)
    _print("api.Child type\n")
elif PROTOCOL == "module-alias-scope-collision":
    a = _base("a")
    b = _base("b")
    a.consume(a.make())
    b.consume(b.make())
elif PROTOCOL == "export-wide-callable":
    main = _base("main")
    values = range({wide_callable_slot_count})
    out = main.select(*values)
    _print(str(out["_0"]) + "\n")
    _print(str(out["_1"]) + "\n")
    _print(str(out["_2"]) + "\n")
    callback_args = {{f"_{{index}}": value for index, value in enumerate(values)}}
    callback_out = main.makeSelect()(callback_args)
    _print(str(callback_out["_0"]) + "\n")
    _print(str(callback_out["_1"]) + "\n")
    _print(str(callback_out["_2"]) + "\n")
else:
    raise RuntimeError("unhandled protocol " + PROTOCOL)
"#,
        module_path = python_string_literal(&package_module.display().to_string()),
        protocol_name = python_string_literal(driver),
        export_root = export_root,
        host_items = host_items,
        host_types = host_types,
        factory = factory,
        wide_callable_slot_count = WIDE_CALLABLE_SLOT_COUNT,
        box_frame = python_string_literal(&exact_host_type_frame("testapi", "Box")),
    )
}

fn host_items_literal(host_api: &HostApi, protocol: RunnerProtocol) -> String {
    let contract = protocol.contract();
    host_api::assert_host_api(contract, host_api);
    let entries = host_api
        .functions
        .iter()
        .zip(contract.host_fns)
        .map(|(entry, binding)| {
            assert_eq!(entry.rendered.name, entry.identity.leaf);
            host_item_literal(&entry.identity, binding, contract.host_types)
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(", "))
}

fn host_types_literal(bindings: &[HostTypeBinding]) -> String {
    let entries = bindings
        .iter()
        .map(|binding| {
            let fixture = match binding.fixture {
                HostTypeFixture::Role(_) => "role",
                HostTypeFixture::SelectedRole(_) => "selected-role",
                HostTypeFixture::Array => "array",
                HostTypeFixture::Box => "box",
                HostTypeFixture::Token => "token",
                HostTypeFixture::Scalar => "scalar",
            };
            format!(
                "({}, {}, {}, {})",
                python_string_literal(binding.module),
                python_string_literal(binding.leaf),
                python_string_literal(&exact_host_type_frame(binding.module, binding.leaf)),
                python_string_literal(fixture),
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(", "))
}

fn host_item_literal(
    identity: &host_api::HostItemIdentity,
    binding: &HostFnBinding,
    host_types: &[HostTypeBinding],
) -> String {
    for role in binding.body.role_refs().into_iter().flatten() {
        role.resolve(host_types);
    }
    let roles = binding
        .body
        .role_refs()
        .into_iter()
        .flatten()
        .map(|role| {
            python_string_literal(&exact_host_type_frame(
                role.identity.module,
                role.identity.leaf,
            ))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let referenced_host_type = match binding.body {
        HostFnBodyKind::Array { array, .. } => Some(array),
        HostFnBodyKind::MakeScalar { scalar, .. }
        | HostFnBodyKind::ScalarOf { scalar, .. }
        | HostFnBodyKind::ScalarAs { scalar, .. }
        | HostFnBodyKind::ScalarIsTrue { scalar, .. } => Some(scalar),
        HostFnBodyKind::MakeToken { token, .. } | HostFnBodyKind::TokenValue { token, .. } => {
            Some(token)
        }
        HostFnBodyKind::BoxGet { box_type } | HostFnBodyKind::BoxMake { box_type } => {
            Some(box_type)
        }
        _ => None,
    };
    let carrier = referenced_host_type
        .map(|identity| {
            python_string_literal(&exact_host_type_frame(identity.module, identity.leaf))
        })
        .unwrap_or_else(|| "None".to_owned());
    let module = (!identity.module.is_empty()).then(|| host_module_key(&identity.module));
    let (tag, operation, kind) = python_host_body_descriptor(binding.body);
    format!(
        "({}, {}, {}, {}, {}, [{}], {})",
        module
            .as_ref()
            .map(|s| python_string_literal(s))
            .unwrap_or_else(|| "None".to_owned()),
        python_string_literal(&host_api::host_name_core(&identity.leaf)),
        python_string_literal(tag),
        operation
            .map(python_string_literal)
            .unwrap_or_else(|| "None".to_owned()),
        kind.map(python_string_literal)
            .unwrap_or_else(|| "None".to_owned()),
        roles,
        carrier,
    )
}

fn exact_host_type_frame(module: &str, leaf: &str) -> String {
    let segments = if module.is_empty() {
        Vec::new()
    } else {
        module.split('/').collect::<Vec<_>>()
    };
    let mut out = format!("V1_M{}_", segments.len());
    for segment in segments {
        let segment = host_api::host_name_core(segment);
        out.push_str(&format!("C{}_{segment}", segment.len()));
    }
    let leaf = host_api::host_name_core(leaf);
    out.push_str(&format!("N{}_{}", leaf.len(), leaf));
    out
}

fn python_role_adapter_identity(module: &str, leaf: &str) -> String {
    let components = module
        .split('/')
        .chain(std::iter::once(leaf))
        .map(host_api::host_name_core)
        .collect::<Vec<_>>();
    if module.is_empty()
        || module.split('/').next() == Some("V1")
        || components.iter().any(|component| {
            component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        exact_host_type_frame(module, leaf)
    } else {
        components.join("_")
    }
}

fn python_host_body_descriptor(
    body: HostFnBodyKind,
) -> (&'static str, Option<&'static str>, Option<&'static str>) {
    match body {
        HostFnBodyKind::Print { .. } => ("print", None, None),
        HostFnBodyKind::Eprint { .. } => ("eprint", None, None),
        HostFnBodyKind::Exit { .. } => ("exit", None, None),
        HostFnBodyKind::ReadAsciiLine { .. } => ("read-ascii-line", None, None),
        HostFnBodyKind::StringConcat { .. } => ("string-concat", None, None),
        HostFnBodyKind::StringEq { .. } => ("string-eq", None, None),
        HostFnBodyKind::StringLen { .. } => ("string-len", None, None),
        HostFnBodyKind::StringSlice { .. } => ("string-slice", None, None),
        HostFnBodyKind::StringCodeAt { .. } => ("string-code-at", None, None),
        HostFnBodyKind::Loop => ("loop", None, None),
        HostFnBodyKind::NumericToString { value, .. } => {
            ("numeric-to-string", None, Some(value.fixture.role()))
        }
        HostFnBodyKind::BoolToString { .. } => ("bool-to-string", None, None),
        HostFnBodyKind::PrintI32 { .. } => ("print-i32", None, None),
        HostFnBodyKind::StringToInt { .. } => ("string-to-int", None, None),
        HostFnBodyKind::Arithmetic { operation, number } => {
            ("arithmetic", Some(operation), Some(number.fixture.role()))
        }
        HostFnBodyKind::FloatArithmetic { operation, number } => (
            "float-arithmetic",
            Some(operation),
            Some(number.fixture.role()),
        ),
        HostFnBodyKind::Compare {
            operation, number, ..
        } => ("compare", Some(operation), Some(number.fixture.role())),
        HostFnBodyKind::Array { operation, .. } => ("array", Some(operation), None),
        HostFnBodyKind::MakeScalar { .. } => ("make-scalar", None, None),
        HostFnBodyKind::ScalarOf { value, .. } => ("scalar-of", None, Some(value.fixture.role())),
        HostFnBodyKind::ScalarAs { value, .. } => ("scalar-as", None, Some(value.fixture.role())),
        HostFnBodyKind::ScalarIsTrue { .. } => ("scalar-is-true", None, None),
        HostFnBodyKind::MakeToken { .. } => ("make-token", None, None),
        HostFnBodyKind::TokenValue { .. } => ("token-value", None, None),
        HostFnBodyKind::BoxGet { .. } => ("box-get", None, None),
        HostFnBodyKind::BoxMake { .. } => ("box-make", None, None),
        HostFnBodyKind::CallStep { .. } => ("call-step", None, None),
        HostFnBodyKind::MakePairCallback { .. } => ("make-pair-callback", None, None),
        HostFnBodyKind::MakeStep { .. } => ("make-step", None, None),
        HostFnBodyKind::ApplyPoly { .. } => ("apply-poly", None, None),
        HostFnBodyKind::MakePairStructural { .. } => ("make-pair-structural", None, None),
        HostFnBodyKind::ProducePair { .. } => ("produce-pair", None, None),
        HostFnBodyKind::SumToString { .. } => ("sum-to-string", None, None),
        HostFnBodyKind::RoundFunctor => ("round-functor", None, None),
        HostFnBodyKind::RoundPicker => ("round-picker", None, None),
        HostFnBodyKind::RoundPolyThunk => ("round-poly-thunk", None, None),
        HostFnBodyKind::RoundPolyUnitSlot => ("round-poly-unit-slot", None, None),
        HostFnBodyKind::StagedSecond { .. } => ("staged-second", None, None),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => ("nested-curried-roundtrip", None, None),
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            ("invoke-substituted-unit-callback", None, None)
        }
        HostFnBodyKind::ReturnedForallUnit => ("returned-forall-unit", None, None),
        HostFnBodyKind::ObservePacked { .. } => ("observe-packed", None, None),
        HostFnBodyKind::TraceUnit { text } => ("trace-unit", Some(text), None),
        HostFnBodyKind::StagedUnitCall => ("staged-unit-call", None, None),
        HostFnBodyKind::UnreachableI32Print { .. } => ("unreachable-i32-print", None, None),
    }
}

fn python_command() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn python_driver(protocol: RunnerProtocol) -> &'static str {
    match protocol.contract().execution {
        ProtocolExecution::CompileOnly => "__compile_only__",
        ProtocolExecution::ConstructOnly => "__construct_only__",
        ProtocolExecution::Invoke(ExportDriver::Main { .. }) => "__main__",
        ProtocolExecution::Invoke(ExportDriver::NamespaceRoundtrip) => "export-namespace-roundtrip",
        ProtocolExecution::Invoke(ExportDriver::CallbackRoundtrip) => "export-callback-roundtrip",
        ProtocolExecution::Invoke(ExportDriver::ModuleRoundtrip) => "export-module-roundtrip",
        ProtocolExecution::Invoke(ExportDriver::MultilabelRoundtrip) => {
            "export-multi-label-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::PolyRoundtrip) => "export-poly-roundtrip",
        ProtocolExecution::Invoke(ExportDriver::PolyCallbackRoundtrip) => {
            "export-poly-callback-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::StructuralRoundtrip) => {
            "export-structural-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::ScalarRoundtrip) => "export-scalar-roundtrip",
        ProtocolExecution::Invoke(ExportDriver::HostOwnedRoundtrip) => {
            "export-host-owned-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::CallableSlotsRoundtrip) => {
            "export-callable-slots-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::RustCallbackAliases) => "rust-callback-aliases",
        ProtocolExecution::Invoke(ExportDriver::FunctorDictRoundtrip) => {
            "export-functor-dict-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::HostExistentialRoundtrip) => {
            "host-existential-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::PositionalProductRoundtrip) => {
            "export-positional-product-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::TypeRoundtrip) => "export-type-roundtrip",
        ProtocolExecution::Invoke(ExportDriver::NewtypeSumRoundtrip) => {
            "export-newtype-sum-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeScalarRoundtrip) => {
            "export-newtype-scalar-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeIgnoredArgumentRoundtrip) => {
            "export-newtype-ignored-argument-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::RecursiveNewtypeBoundary) => {
            "recursive-newtype-boundary"
        }
        ProtocolExecution::Invoke(ExportDriver::NewtypeVisibilityFacade) => {
            "newtype-visibility-facade"
        }
        ProtocolExecution::Invoke(ExportDriver::PublicWordNames) => "public-word-names",
        ProtocolExecution::Invoke(ExportDriver::NestedProductRoundtrip) => {
            "export-nested-product-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::CompoundInputOnce) => "export-compound-input-once",
        ProtocolExecution::Invoke(ExportDriver::CurriedFacade) => "export-curried-facade",
        ProtocolExecution::Invoke(ExportDriver::NestedCurriedRoundtrip) => {
            "nested-curried-roundtrip"
        }
        ProtocolExecution::Invoke(ExportDriver::HostSubstitutedUnitCallback) => {
            "host-substituted-unit-callback"
        }
        ProtocolExecution::Invoke(ExportDriver::ReturnedForallCallByValue) => {
            "returned-forall-call-by-value"
        }
        ProtocolExecution::Invoke(ExportDriver::FacadeSelectorCollisions) => {
            "facade-selector-collisions"
        }
        ProtocolExecution::Invoke(ExportDriver::ModuleAliasScopeCollision) => {
            "module-alias-scope-collision"
        }
        ProtocolExecution::Invoke(ExportDriver::WideCallable) => "export-wide-callable",
        ProtocolExecution::Invoke(ExportDriver::Coexist) => {
            unreachable!("coexist uses the dedicated two-artifact path")
        }
    }
}

fn python_string_literal(s: &str) -> String {
    let mut out = String::from("'");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn package_py_module(dir: &Path, namespace: &str) -> Result<PathBuf, String> {
    let path = dir.join(format!("{namespace}.py"));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "expected Python package module `{}` for artifact namespace `{namespace}`",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!(
            "kio-test-runner-python-{label}-{}-{n}",
            process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn all_protocols() -> &'static [RunnerProtocol] {
        RunnerProtocol::ALL
    }

    fn run_fixture_driver(module_source: &str, protocol: RunnerProtocol) -> process::Output {
        let dir = temp_dir("main-contract");
        let module_path = dir.join("fixture.py");
        fs::write(&module_path, module_source).expect("write fake package");
        let output = Command::new(python_command())
            .arg("-c")
            .arg(build_driver_script(&module_path, "fixture", protocol))
            .output()
            .expect("run python driver");
        let _ = fs::remove_dir_all(&dir);
        output
    }

    #[test]
    fn exact_api_main_is_required_and_invoked() {
        let output = run_fixture_driver(
            "import types\ndef create_fixture(host):\n    return types.SimpleNamespace(api=types.SimpleNamespace(main=lambda: None))\n",
            RunnerProtocol::EmptyApiMain,
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn generated_driver_uses_role_framed_facade_selectors() {
        let script = build_driver_script(
            Path::new("fixture.py"),
            "fixture",
            RunnerProtocol::NewtypeVisibilityFacade,
        );
        assert!(script.contains("'types': 'KioModule_types'"));
        assert!(script.contains("'Constructor_only': 'KioType_ConstructorOnly'"));
    }

    #[test]
    fn public_word_names_driver_pins_affixes_and_nested_selectors() {
        let script = build_driver_script(
            Path::new("fixture.py"),
            "fixture",
            RunnerProtocol::PublicWordNames,
        );
        for literal in [
            "word_api.readWord()",
            "word_api._readWord()",
            "word_api.readWord_()",
            "word_api._readWord_()",
            "word_api.readWord__()",
            "word_nodes.KioType__uWordBox_u_u.wrapWord(55)",
            "word_nodes.KioType__uWordBox_u_u.unwrapWord({\"_WordBox__\": 55})",
            "word_nodes.keepWord(66)",
            "pair = word_api.keepPair({\"_WordBox__\": 77, \"wordApi/otherNodes._WordBox__\": 88})",
        ] {
            assert!(script.contains(literal), "{script}");
        }
    }

    #[test]
    fn selector_collision_oracle_names_readable_and_fallback_role_adapters() {
        let script = build_driver_script(
            Path::new("fixture.py"),
            "fixture",
            RunnerProtocol::FacadeSelectorCollisions,
        );
        assert!(
            script
                .contains("for ns, leaf, _tag, _operation, _kind, _roles, _carrier in HOST_ITEMS")
        );
        assert!(script.contains("expected.add(\"KioHostIn_\" + adapter)"));
        assert!(script.contains("expected.add(\"KioHostOut_\" + adapter)"));
    }

    #[test]
    fn multilabel_driver_uses_the_exact_type_selector() {
        let script = build_driver_script(
            Path::new("fixture.py"),
            "fixture",
            RunnerProtocol::ExportMultilabelRoundtrip,
        );
        assert!(script.contains("'A': 'KioType_A'"));
        assert!(script.contains("a = getattr(main, TYPE_SELECTORS[\"A\"])"));
        assert!(!script.contains("main.A"));
    }

    #[test]
    fn missing_or_non_callable_exact_main_is_an_error() {
        let missing = run_fixture_driver(
            "import types\ndef create_fixture(host):\n    return types.SimpleNamespace(main=types.SimpleNamespace(main=lambda: None))\n",
            RunnerProtocol::EmptyApiMain,
        );
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stderr).contains("missing main module `api`"));

        let non_callable = run_fixture_driver(
            "import types\ndef create_fixture(host):\n    return types.SimpleNamespace(api=types.SimpleNamespace(main=1))\n",
            RunnerProtocol::EmptyApiMain,
        );
        assert!(!non_callable.status.success());
        assert!(
            String::from_utf8_lossy(&non_callable.stderr)
                .contains("main export `api/main` is missing or not callable")
        );
    }

    #[test]
    fn generated_host_constructs_applicable_protocol_bodies_and_rejects_rust_only() {
        let dir = temp_dir("all-host-bodies");
        let module_path = dir.join("fake_pkg.py");
        fs::write(
            &module_path,
            "def create_fake_pkg(host):\n    return object()\n",
        )
        .expect("write fake package");

        for protocol in all_protocols() {
            if protocol.contract().execution == ProtocolExecution::Invoke(ExportDriver::Coexist) {
                continue;
            }
            let script = build_driver_script(&module_path, "fixture", *protocol).replace(
                "\nmodule = _load_package(MODULE_PATH)",
                "\nsys.exit(0)\nmodule = _load_package(MODULE_PATH)",
            );
            let output = Command::new(python_command())
                .arg("-c")
                .arg(script)
                .output()
                .expect("run python3");

            if *protocol == RunnerProtocol::RustCallbackAliases {
                assert!(!output.status.success(), "Rust-only protocol was accepted");
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains(
                        "rust-callback-aliases tests the Rust public naming contract only"
                    ),
                    "unexpected rejection: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                continue;
            }

            assert!(
                output.status.success(),
                "python driver failed while constructing host for {}:\nstdout:\n{}\nstderr:\n{}",
                protocol.name(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_body_descriptors_do_not_depend_on_function_leaf() {
        let string = protocol::HostRoleRef::new("testapi", "String", protocol::RoleFixture::String);
        let i64 = protocol::HostRoleRef::new("testapi", "I64", protocol::RoleFixture::I64);
        let host_types = [
            HostTypeBinding::role("testapi", "String", protocol::RoleFixture::String),
            HostTypeBinding::role("testapi", "I64", protocol::RoleFixture::I64),
        ];
        let string_concat = HostFnBinding {
            module: "odd/module",
            leaf: "add_i128",
            body: HostFnBodyKind::StringConcat { string },
        };
        let string_concat_identity =
            host_api::HostItemIdentity::new(string_concat.module, string_concat.leaf);
        assert_eq!(
            host_item_literal(&string_concat_identity, &string_concat, &host_types),
            "('odd_module', 'addI128', 'string-concat', None, None, ['V1_M1_C7_testapiN6_String'], None)"
        );

        let arithmetic = HostFnBinding {
            module: "",
            leaf: "print",
            body: HostFnBodyKind::Arithmetic {
                operation: "mul",
                number: i64,
            },
        };
        let arithmetic_identity =
            host_api::HostItemIdentity::new(arithmetic.module, arithmetic.leaf);
        assert_eq!(
            host_item_literal(&arithmetic_identity, &arithmetic, &host_types),
            "(None, 'print', 'arithmetic', 'mul', 'i64', ['V1_M1_C7_testapiN3_I64'], None)"
        );
    }

    #[test]
    fn selected_role_host_types_get_distinct_exact_type_frames() {
        let contract = RunnerProtocol::HostTypeRoundtrip.contract();
        let literal = host_types_literal(contract.host_types);
        assert!(literal.contains("'V1_M1_C7_testapiN5_Count', 'selected-role'"));
        assert!(literal.contains("'V1_M1_C7_testapiN3_I32', 'selected-role'"));
        assert!(literal.contains("'V1_M1_C7_testapiN6_String', 'selected-role'"));
        assert_ne!(
            exact_host_type_frame("testapi", "Count"),
            exact_host_type_frame("testapi", "I32")
        );
    }

    #[test]
    fn selected_role_driver_keeps_equal_primitive_roles_distinct() {
        let count = python_role_adapter_identity("testapi", "Count");
        let i32 = python_role_adapter_identity("testapi", "I32");
        let string = python_role_adapter_identity("testapi", "String");
        let module = format!(
            r#"import types
def create_fixture(host):
    def main():
        count = getattr(host, "KioHostOut_{count}")(40)
        two = getattr(host, "KioHostOut_{count}")(2)
        exact_i32 = getattr(host, "KioHostOut_{i32}")(37)
        if type(count) is type(exact_i32):
            raise RuntimeError("Count and I32 aliases collapsed")
        total = host.testapi_arith.addCount(count, two)
        if getattr(host, "KioHostIn_{count}")(total) != 42:
            raise RuntimeError("Count adapter lost the host result")
        rendered = host.testapi_fmt.countToString(total)
        if getattr(host, "KioHostIn_{string}")(rendered) != "42":
            raise RuntimeError("String adapter lost the formatter result")
        host.testapi_io.print(rendered)
    return types.SimpleNamespace(
        testapi=types.SimpleNamespace(KioModule_main=types.SimpleNamespace(main=main)))
"#
        );
        let output = run_fixture_driver(&module, RunnerProtocol::HostTypeRoundtrip);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "42");
    }

    #[test]
    fn parameterized_host_fixture_uses_the_emitted_declaration_carrier() {
        let box_frame = exact_host_type_frame("testapi", "Box");
        let module = format!(
            r#"import types
class Carrier:
    def __init__(self, value): self.value = value
    def __class_getitem__(cls, _args): return cls
    @classmethod
    def from_native(cls, value): return cls(value)
    def to_native(self): return self.value
KioHostType_{box_frame} = Carrier
def create_fixture(host):
    def main():
        box = host.testapi_arith.boxMake(42)
        if type(box) is not Carrier:
            raise RuntimeError("Box did not use the emitted carrier")
        if host.testapi_arith.boxGet(box) != 42:
            raise RuntimeError("Box carrier did not round-trip")
    return types.SimpleNamespace(
        testapi=types.SimpleNamespace(KioModule_main=types.SimpleNamespace(main=main)))
"#
        );
        let output = run_fixture_driver(&module, RunnerProtocol::HostGenericTypeRoundtrip);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

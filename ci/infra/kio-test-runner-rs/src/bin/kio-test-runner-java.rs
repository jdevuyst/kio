//! `kio-test-runner-java` — pointed at a `kio build java` output
//! directory, compiles the emitted typed facade together with a
//! synthesized typed driver and reports the exit code.
//!
//! The package layout the emitter produces (see `specs/backends/java.md`
//! § Output layout) is a namespace directory holding `<Handle>.java`,
//! `<Handle>Host.java`, `Shapes.java`, and `KioRuntime.java`. The corpus
//! harness supplies the artifact namespace independently of emitted
//! source; the runner derives the handle from that namespace, drops a
//! `Driver.java` beside the copied tree in a temp build dir, and runs
//! `javac` + `java`.
//!
//! The protocol is the sole semantic authority: it supplies the exact host
//! types and functions, their native fixtures and bodies, and the export
//! driver. The runner never reads the emitted interface to discover or
//! filter members. Its `StubHost` implements the protocol contract
//! method-for-method; if the emitted FFI drifts, `javac` fails. Shaped slots
//! are spelled through the documented generic semantic shells in `Shapes`
//! (see `specs/backends/java.md` § FFI surface), never by scraping emitted
//! source.
//!
//! Exits 0 on success, 1 on a `javac` / runtime error; the CLI tier is 2
//! per `specs/exit-codes.md`. A module call to host `exit(n)` propagates
//! `n` through the spawned JVM's exit code (clamped to 0..=125).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use kio_ci_scheduler as compiler_admission;

#[path = "../shared/artifact_identity.rs"]
mod artifact_identity;
#[path = "../shared/canonical.rs"]
mod canonical;
#[path = "../shared/compiler_observer.rs"]
mod compiler_observer;
#[path = "../shared/host_api.rs"]
mod host_api;
#[path = "../shared/protocol.rs"]
mod protocol;
#[path = "../shared/runner.rs"]
mod runner;

use artifact_identity::{ArtifactIdentity, ArtifactIdentityArgs, pascal_case};
use canonical::{ArrayOp, CanonicalKind};
use compiler_observer::CompilerObserver;
use host_api::{
    AssocType, HostApi, TraitMethod, facade_module_selector, facade_type_selector, java_host_member,
};
use protocol::{
    ExportDriver, HostFnBinding, HostFnBodyKind, HostRoleRef, HostTypeBinding, HostTypeFixture,
    ProtocolExecution, RoleFixture, RunnerProtocol, WIDE_CALLABLE_SLOT_COUNT,
};
use runner::{EXIT_RUNTIME_FAILURE, EXIT_USAGE, TestRunner};

const USAGE: &str = "\
Usage: kio-test-runner-java [--protocol <name>] <output-dir>

Compile and run the Java package emitted by `kio build java`.

Arguments:
  <output-dir>   Directory containing the emitted namespace tree
                 (`<ns>/<Handle>.java`, `<ns>/<Handle>Host.java`,
                 `<ns>/Shapes.java`, `<ns>/KioRuntime.java`) per
                 `specs/backends/java.md`.

Environment:
  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER
                 Optional internal debug executable placed outermost
                 around each actual `javac` compile. The value is one
                 opaque executable, not shell syntax.

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
  -h, --help     Show this help and exit.
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

    let coexist = matches!(
        protocol.contract().execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist)
    );
    let expected_packages = if coexist { 2 } else { 1 };
    let identities = match identity_args.resolve("java", expected_packages) {
        Ok(identities) => identities,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let compiler_admission = match compiler_admission::CompilerAdmission::from_env() {
        Ok(admission) => admission,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let compiler_observer = match CompilerObserver::from_env() {
        Ok(observer) => observer,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    // The coexist protocol is the one two-artifact protocol: exactly two
    // positional output dirs; every other protocol takes exactly one.
    if coexist {
        return match positional.as_slice() {
            [a, b] => match run_coexist(
                Path::new(*a),
                &identities[0],
                Path::new(*b),
                &identities[1],
                &compiler_observer,
                &compiler_admission,
            ) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: executing coexist artifacts: {e}");
                    EXIT_RUNTIME_FAILURE
                }
            },
            _ => {
                eprintln!("error: --protocol coexist takes exactly two <output-dir> arguments");
                EXIT_USAGE
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

    JavaRunner {
        compiler_observer,
        compiler_admission,
        protocol,
        identity: identities.into_iter().next().unwrap(),
    }
    .run(dir, protocol)
}

/// The `coexist` protocol's two-artifact execution (`shared/protocol.rs`
/// § The coexist protocol): both package trees compile in one `javac`
/// invocation — the maximal-collision shape the namespace rule exists
/// for — and one driver hosts both behind independent implementations of each
/// artifact's exact generic host interface. Their `print` methods prefix the
/// package's namespace.
fn run_coexist(
    dir_a: &Path,
    identity_a: &ArtifactIdentity,
    dir_b: &Path,
    identity_b: &ArtifactIdentity,
    compiler_observer: &CompilerObserver,
    compiler_admission: &compiler_admission::CompilerAdmission,
) -> Result<i32, String> {
    let a = emitted_package(dir_a, identity_a)?;
    let b = emitted_package(dir_b, identity_b)?;
    if a.namespace == b.namespace {
        return Err(format!(
            "coexist requires two distinct package namespaces; both artifacts are `{}`",
            a.namespace
        ));
    }
    let build_dir = temp_dir("coexist")?;
    let mut java_files: Vec<PathBuf> = Vec::new();
    copy_package_tree(&a, &build_dir, &mut java_files)?;
    copy_package_tree(&b, &build_dir, &mut java_files)?;
    let contract = RunnerProtocol::Coexist.contract();
    let root_arguments = java_root_binding_arguments(contract);
    let root_use = if root_arguments.is_empty() {
        String::new()
    } else {
        format!("<{}>", root_arguments.join(", "))
    };
    let mut driver = String::new();
    driver.push_str("public final class Driver {\n");
    driver.push_str(&render_coexist_host(
        "HostA",
        &a.namespace,
        &a.handle,
        "first: ",
        contract,
    ));
    driver.push_str(&render_coexist_host(
        "HostB",
        &b.namespace,
        &b.handle,
        "second: ",
        contract,
    ));
    driver.push_str("  public static void main(String[] args) {\n");
    driver.push_str(&format!(
        "    {ns_a}.{h_a}{root_use} pa = {ns_a}.{h_a}.create(new HostA());\n",
        ns_a = a.namespace,
        h_a = a.handle,
    ));
    driver.push_str(&format!(
        "    {ns_b}.{h_b}{root_use} pb = {ns_b}.{h_b}.create(new HostB());\n",
        ns_b = b.namespace,
        h_b = b.handle,
    ));
    // Both packages export the same `pair() -> (I32 & String)`. Reading the
    // generic product shell through both live package handles proves that
    // equal structural facades coexist without coupling artifact identity.
    let main = facade_module_selector("main", false);
    driver.push_str(&format!(
        "    pa.greeter.{main}.main();\n    pb.greeter.{main}.main();\n    pa.greeter.{main}.main();\n    var qa = pa.greeter.{main}.pair();\n    System.out.println(\"first pair: \" + qa._0() + \" \" + qa._1());\n    var qb = pb.greeter.{main}.pair();\n    System.out.println(\"second pair: \" + qb._0() + \" \" + qb._1());\n    qa = pa.greeter.{main}.pair();\n    System.out.println(\"first pair: \" + qa._0() + \" \" + qa._1());\n  }}\n}}\n"
    ));
    fs::write(build_dir.join("Driver.java"), driver)
        .map_err(|e| format!("writing Java driver in {}: {e}", build_dir.display()))?;
    java_files.push(PathBuf::from("Driver.java"));
    compile_and_run_driver(
        &build_dir,
        &java_files,
        compiler_observer,
        compiler_admission,
        &[],
    )
}

fn render_coexist_host(
    class_name: &str,
    namespace: &str,
    handle: &str,
    prefix: &str,
    contract: protocol::ProtocolContract,
) -> String {
    assert_eq!(
        contract.execution,
        ProtocolExecution::Invoke(ExportDriver::Coexist),
        "the Java coexist host renderer accepts only the coexist contract"
    );
    let root_arguments = java_root_binding_arguments(contract);
    let root_use = if root_arguments.is_empty() {
        String::new()
    } else {
        format!("<{}>", root_arguments.join(", "))
    };
    let mut out = format!(
        "  static final class {class_name} implements {namespace}.{handle}Host{root_use} {{\n"
    );
    for binding in contract.host_types {
        out.push_str(&render_host_binding_adapters(binding));
        out.push('\n');
    }
    let print = contract
        .host_fns
        .iter()
        .find(|binding| matches!(binding.body, HostFnBodyKind::Print { .. }))
        .unwrap_or_else(|| unreachable!("the coexist protocol has no print host function"));
    let method = java_method(print, contract.host_types);
    assert_eq!(method.arg_types.len(), 1, "coexist print has one argument");
    out.push_str(&format!(
        "  @Override\n  public void {}({} p0) {{\n    System.out.print({prefix:?} + p0);\n  }}\n",
        method.name, method.arg_types[0]
    ));
    out.push_str("  }\n\n");
    out
}

/// Copy an emitted package's namespace tree into the build dir (javac
/// wants the source layout intact), appending each copied file's
/// build-dir-relative path to `java_files`.
fn copy_package_tree(
    artifact: &EmittedPackage,
    build_dir: &Path,
    java_files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let ns_rel: PathBuf = artifact.namespace.split('.').collect();
    let dest_ns_dir = build_dir.join(&ns_rel);
    fs::create_dir_all(&dest_ns_dir)
        .map_err(|e| format!("creating {}: {e}", dest_ns_dir.display()))?;
    let mut copied = 0usize;
    for entry in fs::read_dir(&artifact.ns_dir)
        .map_err(|e| format!("reading {}: {e}", artifact.ns_dir.display()))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "java") {
            let name = path.file_name().unwrap();
            let dest = dest_ns_dir.join(name);
            fs::copy(&path, &dest)
                .map_err(|e| format!("copying {} into build dir: {e}", path.display()))?;
            java_files.push(ns_rel.join(name));
            copied += 1;
        }
    }
    if copied == 0 {
        return Err(format!("no .java files in {}", artifact.ns_dir.display()));
    }
    Ok(())
}

/// `javac` the collected sources in `build_dir`, run `Driver`, and
/// return the process exit code. Shared by the one-artifact path and
/// the coexist path.
fn compile_and_run_driver(
    build_dir: &Path,
    java_files: &[PathBuf],
    compiler_observer: &CompilerObserver,
    compiler_admission: &compiler_admission::CompilerAdmission,
    java_args: &[&str],
) -> Result<i32, String> {
    let mut compile = javac_compile_command(compiler_observer);
    for f in java_files {
        compile.arg(f);
    }
    compile.current_dir(build_dir);
    let admitted = compiler_admission
        .acquire_for(&mut compile)
        .map_err(|e| format!("acquiring compiler admission: {e}"))?;
    let compile = admitted
        .output()
        .map_err(|e| format!("spawning {}: {e}", javac_command()))?;
    if !compile.status.success() {
        return Err(format!(
            "javac failed in {}:\nstdout:\n{}\nstderr:\n{}",
            build_dir.display(),
            String::from_utf8_lossy(&compile.stdout),
            String::from_utf8_lossy(&compile.stderr)
        ));
    }
    let mut child = Command::new(java_command())
        .args(java_args)
        .arg("-cp")
        .arg(build_dir)
        .arg("Driver")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawning {}: {e}", java_command()))?;
    let status = child
        .wait()
        .map_err(|e| format!("waiting for {}: {e}", java_command()))?;
    let _ = fs::remove_dir_all(build_dir);
    Ok(status.code().unwrap_or(EXIT_RUNTIME_FAILURE))
}

fn javac_compile_command(compiler_observer: &CompilerObserver) -> Command {
    compiler_observer.command(std::ffi::OsStr::new(javac_command()), None)
}

struct JavaRunner {
    compiler_observer: CompilerObserver,
    compiler_admission: compiler_admission::CompilerAdmission,
    protocol: RunnerProtocol,
    identity: ArtifactIdentity,
}

impl TestRunner for JavaRunner {
    fn host_api(&self) -> HostApi {
        java_host_api_for_protocol(self.protocol)
    }

    fn execute_artifact(
        &self,
        output_dir: &Path,
        host_api: &HostApi,
        protocol: RunnerProtocol,
    ) -> Result<i32, String> {
        let artifact = emitted_package(output_dir, &self.identity)?;
        let build_dir = temp_dir("build")?;
        let mut java_files: Vec<PathBuf> = Vec::new();
        copy_package_tree(&artifact, &build_dir, &mut java_files)?;
        fs::write(
            build_dir.join("Driver.java"),
            build_driver_source(host_api, protocol, &artifact),
        )
        .map_err(|e| format!("writing Java driver in {}: {e}", build_dir.display()))?;
        java_files.push(PathBuf::from("Driver.java"));
        let java_args = if protocol == RunnerProtocol::FacadeSelectorCollisions {
            &["-Duser.language=tr", "-Duser.country=TR"][..]
        } else {
            &[]
        };
        compile_and_run_driver(
            &build_dir,
            &java_files,
            &self.compiler_observer,
            &self.compiler_admission,
            java_args,
        )
    }
}

/// The exact emitted package location derived from the independently
/// supplied artifact identity.
struct EmittedPackage {
    namespace: String,
    handle: String,
    ns_dir: PathBuf,
}

fn emitted_package(dir: &Path, identity: &ArtifactIdentity) -> Result<EmittedPackage, String> {
    let final_segment = identity
        .namespace
        .rsplit('.')
        .next()
        .expect("a validated namespace is non-empty");
    let handle = pascal_case(final_segment);
    let ns_rel: PathBuf = identity.namespace.split('.').collect();
    let ns_dir = dir.join(ns_rel);
    let handle_file = ns_dir.join(format!("{handle}.java"));
    let host_iface = ns_dir.join(format!("{handle}Host.java"));
    if !handle_file.is_file() || !host_iface.is_file() {
        return Err(format!(
            "expected Java facade pair `{}` and `{}` for artifact namespace `{}`",
            handle_file.display(),
            host_iface.display(),
            identity.namespace
        ));
    }
    Ok(EmittedPackage {
        namespace: identity.namespace.clone(),
        handle,
        ns_dir,
    })
}

// =========================================================================
// Host API construction (Java-typed).
// =========================================================================

/// The runner-selected boxed Java binding for one role declaration. Java
/// generics cannot carry primitives, so even canonical fixtures use their
/// reference counterparts at the public facade.
fn java_role_type(role: RoleFixture) -> &'static str {
    match role {
        RoleFixture::I8 => "Byte",
        RoleFixture::I16 => "Short",
        RoleFixture::I32 => "Integer",
        RoleFixture::I64 => "Long",
        RoleFixture::U8 => "Short",
        RoleFixture::U16 => "Integer",
        RoleFixture::U32 => "Long",
        RoleFixture::I128 | RoleFixture::U64 | RoleFixture::U128 => "java.math.BigInteger",
        RoleFixture::F32 => "Float",
        RoleFixture::F64 => "Double",
        RoleFixture::Bool => "Boolean",
        RoleFixture::String => "String",
    }
}

fn java_selected_role_type(binding: &HostTypeBinding) -> String {
    format!(
        "KioSelected_{}",
        java_host_type_identity(binding.module, binding.leaf)
    )
}

fn java_role_ref_type(role: HostRoleRef, host_types: &[HostTypeBinding]) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::Role(role) => java_role_type(role).to_owned(),
        HostTypeFixture::SelectedRole(_) => java_selected_role_type(binding),
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

fn java_host_type_identity(module: &str, leaf: &str) -> String {
    fn encode(source: &str) -> String {
        let source = source
            .split('/')
            .map(host_api::host_name_core)
            .collect::<Vec<_>>()
            .join("/");
        let mut encoded = String::with_capacity(source.len());
        for byte in source.bytes() {
            match byte {
                b'_' => encoded.push_str("_u"),
                b'/' => encoded.push_str("_s"),
                _ => encoded.push(char::from(byte)),
            }
        }
        encoded
    }
    format!("{}__{}", encode(module), encode(leaf))
}

fn java_host_carrier(binding: &HostTypeBinding) -> String {
    format!(
        "Shapes.KioHostType_{}",
        java_host_type_identity(binding.module, binding.leaf)
    )
}

fn java_nullary_host_selection(binding: &HostTypeBinding) -> String {
    assert_eq!(
        binding.type_arity, 0,
        "a Java package-root selection must be nullary"
    );
    match binding.fixture {
        HostTypeFixture::Role(role) => java_role_type(role).to_owned(),
        HostTypeFixture::SelectedRole(_) => java_selected_role_type(binding),
        HostTypeFixture::Token => "TokenValue".to_owned(),
        HostTypeFixture::Scalar => "java.util.Map<String, Object>".to_owned(),
        HostTypeFixture::Array | HostTypeFixture::Box => {
            unreachable!("a parameterized Java fixture cannot be a nullary root selection")
        }
    }
}

fn java_host_binding(
    identity: protocol::HostTypeIdentity,
    host_types: &[HostTypeBinding],
) -> &HostTypeBinding {
    let mut matches = host_types
        .iter()
        .filter(|binding| binding.module == identity.module && binding.leaf == identity.leaf);
    let binding = matches.next().unwrap_or_else(|| {
        unreachable!(
            "Java protocol references undeclared host type `{}/{}`",
            identity.module, identity.leaf
        )
    });
    assert!(
        matches.next().is_none(),
        "Java protocol host type `{}/{}` resolves more than once",
        identity.module,
        identity.leaf
    );
    binding
}

fn java_host_ref_type(
    identity: protocol::HostTypeIdentity,
    host_types: &[HostTypeBinding],
) -> String {
    java_nullary_host_selection(java_host_binding(identity, host_types))
}

fn java_newtype_type(
    module: &str,
    leaf: &str,
    host_types: &[HostTypeBinding],
    declaration_arguments: &[&str],
) -> String {
    let mut arguments = host_types
        .iter()
        .filter(|binding| binding.type_arity == 0)
        .collect::<Vec<_>>();
    arguments.sort_by_key(|binding| (binding.module, binding.leaf));
    let arguments = arguments
        .into_iter()
        .map(java_nullary_host_selection)
        .chain(
            declaration_arguments
                .iter()
                .map(|argument| (*argument).to_owned()),
        )
        .collect::<Vec<_>>();
    let carrier = format!(
        "Shapes.KioNewtype_{}",
        java_host_type_identity(module, leaf)
    );
    if arguments.is_empty() {
        carrier
    } else {
        format!("{carrier}<{}>", arguments.join(", "))
    }
}

fn java_newtype_constructor_marker(
    module: &str,
    leaf: &str,
    host_types: &[HostTypeBinding],
) -> String {
    let marker = format!(
        "Shapes.KioNewtypeMk_{}",
        java_host_type_identity(module, leaf)
    );
    let mut roots = host_types
        .iter()
        .filter(|binding| binding.type_arity == 0)
        .collect::<Vec<_>>();
    roots.sort_by_key(|binding| (binding.module, binding.leaf));
    let roots = roots
        .into_iter()
        .map(java_nullary_host_selection)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        marker
    } else {
        format!("{marker}<{}>", roots.join(", "))
    }
}

fn java_positional_shell(kind: &str, arity: usize) -> String {
    if arity == 2 {
        return kind.to_owned();
    }
    format!("KioFacade_V1_{kind}_Positional_K{arity}")
}

#[cfg(test)]
fn java_bare_shell(kind: &str, keys: &[&str]) -> String {
    let encoded_keys = keys
        .iter()
        .map(|key| {
            let encoded = key
                .bytes()
                .map(|byte| match byte {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => char::from(byte).to_string(),
                    b'_' => "_u".to_owned(),
                    _ => format!("_x{byte:02x}"),
                })
                .collect::<String>();
            format!("B{}_{encoded}", encoded.len())
        })
        .collect::<String>();
    format!("KioFacade_V1_{kind}_K{}_{encoded_keys}", keys.len())
}

fn java_structural_type(kind: &str, types: &[&str]) -> String {
    format!(
        "Shapes.{}<{}>",
        java_positional_shell(kind, types.len()),
        types.join(", ")
    )
}

fn java_product_type(types: &[&str]) -> String {
    java_structural_type("Product", types)
}

fn java_sum_type(types: &[&str]) -> String {
    java_structural_type("Sum", types)
}

fn java_function_type(params: &[&str], ret: &str) -> String {
    let mut args = params.to_vec();
    args.push(ret);
    format!("Shapes.Fn{}<{}>", params.len(), args.join(", "))
}

fn java_apply_poly_type(binding: &HostFnBinding, host_types: &[HostTypeBinding]) -> String {
    const INTERFACE: &str =
        "Shapes.KioForall_Site_M_testapi_sarith__H_apply_upoly_Binders_K1_B0_A0";
    assert_eq!(
        (binding.module, binding.leaf),
        ("testapi/arith", "apply_poly"),
        "the Java rank-N API oracle names its exact protocol site"
    );
    let mut roots = host_types
        .iter()
        .filter(|host_type| host_type.type_arity == 0)
        .collect::<Vec<_>>();
    roots.sort_by_key(|host_type| (host_type.module, host_type.leaf));
    let roots = roots
        .into_iter()
        .map(java_nullary_host_selection)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        INTERFACE.to_owned()
    } else {
        format!("{INTERFACE}<{}>", roots.join(", "))
    }
}

fn java_returned_forall_type(binding: &HostFnBinding) -> String {
    const INTERFACE: &str = "Shapes.KioForall_Site_M_testapi_smain__H_produce_Binders_K1_B0_A0";
    assert_eq!(
        (binding.module, binding.leaf),
        ("testapi/main", "produce"),
        "the Java returned-forall oracle names its exact protocol site"
    );
    INTERFACE.to_owned()
}

/// The protocol's exact [`HostApi`], rendered for Java. Member identity,
/// native fixtures, and the function inventory all come from
/// [`RunnerProtocol::contract`].
fn java_host_api_for_protocol(protocol: RunnerProtocol) -> HostApi {
    let contract = protocol.contract();
    host_api::project_host_api(
        contract,
        |binding| AssocType {
            name: binding.leaf.to_owned(),
            boundary_name: (!binding.module.is_empty())
                .then(|| java_host_member(binding.module, binding.leaf)),
            role: match binding.fixture {
                HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => {
                    role.role().to_owned()
                }
                _ => String::new(),
            },
            type_params: (0..binding.type_arity)
                .map(|index| format!("T{index}"))
                .collect(),
        },
        |binding| java_method(binding, contract.host_types),
    )
}

fn java_method(binding: &HostFnBinding, host_types: &[HostTypeBinding]) -> TraitMethod {
    let module = (!binding.module.is_empty()).then_some(binding.module);
    let member = match module {
        Some(module) => java_host_member(module, binding.leaf),
        None => binding.leaf.to_owned(),
    };
    let method = |args: Vec<String>, ret: &str| TraitMethod {
        name: member.clone(),
        type_params: Vec::new(),
        arg_types: args,
        ret_type: ret.to_owned(),
        where_clause: String::new(),
    };
    let role = |role: HostRoleRef| java_role_ref_type(role, host_types);
    match binding.body {
        HostFnBodyKind::CallStep { i32, string, bool_ } => {
            let i32 = role(i32);
            let string = role(string);
            let bool_ = role(bool_);
            method(
                vec![
                    java_function_type(&[&i32, &string, &bool_], &i32),
                    i32.clone(),
                ],
                &i32,
            )
        }
        HostFnBodyKind::MakePairCallback { i32, string } => {
            let i32 = role(i32);
            let string = role(string);
            let product = java_product_type(&[&i32, &string]);
            method(
                vec![java_function_type(&[&i32], &product), i32.clone()],
                &i32,
            )
        }
        HostFnBodyKind::MakeStep { i32 } => {
            let i32 = role(i32);
            method(vec![i32.clone()], &java_function_type(&[&i32], &i32))
        }
        HostFnBodyKind::BoxMake { box_type } => {
            let carrier = format!(
                "{}<KioCallType_0>",
                java_host_carrier(java_host_binding(box_type, host_types))
            );
            TraitMethod {
                name: member,
                type_params: vec!["KioCallType_0".to_owned()],
                arg_types: vec!["KioCallType_0".to_owned()],
                ret_type: carrier,
                where_clause: String::new(),
            }
        }
        HostFnBodyKind::BoxGet { box_type } => {
            let carrier = format!(
                "{}<KioCallType_0>",
                java_host_carrier(java_host_binding(box_type, host_types))
            );
            TraitMethod {
                name: member,
                type_params: vec!["KioCallType_0".to_owned()],
                arg_types: vec![carrier],
                ret_type: "KioCallType_0".to_owned(),
                where_clause: String::new(),
            }
        }
        HostFnBodyKind::ApplyPoly { string } => {
            let string = role(string);
            method(vec![java_apply_poly_type(binding, host_types)], &string)
        }
        HostFnBodyKind::MakeToken { value_i32, token } => {
            let token = java_host_ref_type(token, host_types);
            method(vec![role(value_i32)], &token)
        }
        HostFnBodyKind::TokenValue { token, value_i32 } => {
            let value_i32 = role(value_i32);
            method(vec![java_host_ref_type(token, host_types)], &value_i32)
        }
        HostFnBodyKind::RoundFunctor => {
            let box_constructor =
                java_newtype_constructor_marker("testapi/types", "Box", host_types);
            let functor =
                java_newtype_type("testapi/types", "Functor", host_types, &[&box_constructor]);
            method(vec![functor.clone()], &functor)
        }
        HostFnBodyKind::RoundPicker => {
            let picker = java_newtype_type("testapi/types", "Pick_first", host_types, &[]);
            method(vec![picker.clone()], &picker)
        }
        HostFnBodyKind::RoundPolyThunk => {
            let thunk = java_newtype_type("testapi/types", "Poly_thunk", host_types, &[]);
            method(vec![thunk.clone()], &thunk)
        }
        HostFnBodyKind::RoundPolyUnitSlot => {
            let slot =
                java_newtype_type("testapi/types", "Unit_slot", host_types, &["Shapes.Unit"]);
            method(vec![slot.clone()], &slot)
        }
        HostFnBodyKind::NestedCurriedRoundtrip { string } => {
            let string = role(string);
            let inner = java_function_type(&[&string], &string);
            let callback = java_function_type(&[&string], &inner);
            method(vec![callback.clone()], &callback)
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { text } => {
            let text = role(text);
            let callback = java_function_type(&["Shapes.Unit"], &text);
            method(vec![callback], &text)
        }
        HostFnBodyKind::ReturnedForallUnit => {
            method(Vec::new(), &java_returned_forall_type(binding))
        }
        HostFnBodyKind::ObservePacked { i32 } => method(
            vec![java_newtype_type(
                "testapi/types",
                "Packed",
                host_types,
                &[],
            )],
            &role(i32),
        ),
        HostFnBodyKind::TraceUnit { .. } => method(Vec::new(), "()"),
        HostFnBodyKind::StagedUnitCall => method(vec!["Object".to_owned()], "()"),
        HostFnBodyKind::StagedSecond { string } => {
            let string = role(string);
            method(vec![string.clone(), string.clone()], &string)
        }
        HostFnBodyKind::MakePairStructural { i32, string } => {
            let i32 = role(i32);
            let string = role(string);
            method(
                vec![i32.clone(), string.clone()],
                &java_product_type(&[&i32, &string]),
            )
        }
        HostFnBodyKind::ProducePair { i32, string } => {
            method(Vec::new(), &java_product_type(&[&role(i32), &role(string)]))
        }
        HostFnBodyKind::SumToString { string, i32 } => {
            let string = role(string);
            let i32 = role(i32);
            method(vec![java_sum_type(&[&i32, &string])], &string)
        }
        HostFnBodyKind::UnreachableI32Print { i32 } => method(vec![role(i32)], "()"),
        _ => java_canonical_method(binding, host_types),
    }
}

/// Build one canonical host fn's Java-typed [`TraitMethod`].
fn java_canonical_method(binding: &HostFnBinding, host_types: &[HostTypeBinding]) -> TraitMethod {
    let module = (!binding.module.is_empty()).then_some(binding.module);
    let member = |l: &str| match module {
        Some(m) => java_host_member(m, l),
        None => l.to_owned(),
    };
    let m = |args: Vec<String>, ret: String| TraitMethod {
        name: member(binding.leaf),
        type_params: Vec::new(),
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let role = |role: HostRoleRef| java_role_ref_type(role, host_types);
    match binding.body {
        HostFnBodyKind::Print { string } | HostFnBodyKind::Eprint { string } => {
            m(vec![role(string)], "()".to_owned())
        }
        HostFnBodyKind::PrintI32 { value } => m(vec![role(value)], "()".to_owned()),
        HostFnBodyKind::Exit { status_i32 } => m(vec![role(status_i32)], "Void".to_owned()),
        HostFnBodyKind::NumericToString { value, string } => m(vec![role(value)], role(string)),
        HostFnBodyKind::BoolToString { bool_, string } => m(vec![role(bool_)], role(string)),
        HostFnBodyKind::StringConcat { string } => {
            let string = role(string);
            m(vec![string.clone(), string.clone()], string)
        }
        HostFnBodyKind::StringEq { string, bool_ } => {
            let string = role(string);
            m(vec![string.clone(), string], role(bool_))
        }
        HostFnBodyKind::StringLen { string, index } => m(vec![role(string)], role(index)),
        HostFnBodyKind::StringSlice { string, index } => {
            let index = role(index);
            m(vec![role(string), index.clone(), index], role(string))
        }
        HostFnBodyKind::StringCodeAt { string, index } => {
            let index = role(index);
            m(
                vec![role(string), index.clone()],
                java_sum_type(&[&index, "Shapes.Unit"]),
            )
        }
        HostFnBodyKind::StringToInt { string, int } => {
            let int = role(int);
            m(vec![role(string)], java_sum_type(&[&int, "Shapes.Unit"]))
        }
        HostFnBodyKind::ReadAsciiLine { string } => {
            let string = role(string);
            m(vec![], java_sum_type(&[&string, "Shapes.Unit"]))
        }
        HostFnBodyKind::Arithmetic { number, .. }
        | HostFnBodyKind::FloatArithmetic { number, .. } => {
            let native = role(number);
            m(vec![native.clone(), native.clone()], native)
        }
        HostFnBodyKind::Compare { number, bool_, .. } => {
            let native = role(number);
            m(vec![native.clone(), native], role(bool_))
        }
        HostFnBodyKind::Loop => TraitMethod {
            name: member(binding.leaf),
            type_params: vec!["s".to_owned(), "r".to_owned()],
            arg_types: vec![
                java_function_type(&["s"], &java_sum_type(&["s", "r"])),
                "s".to_owned(),
            ],
            ret_type: "r".to_owned(),
            where_clause: format!("__cbret={}", java_sum_type(&["s", "r"])),
        },
        HostFnBodyKind::Array {
            operation,
            array,
            index,
        } => java_array_method(binding.leaf, operation, array, index, host_types, &member),
        HostFnBodyKind::MakeScalar { string, scalar } => {
            let string = role(string);
            m(
                vec![string.clone(), string],
                java_host_ref_type(scalar, host_types),
            )
        }
        HostFnBodyKind::ScalarOf { value, scalar } => {
            m(vec![role(value)], java_host_ref_type(scalar, host_types))
        }
        HostFnBodyKind::ScalarAs { value, scalar } => {
            let fixture = value.fixture;
            let value = role(value);
            let ret = match fixture {
                RoleFixture::I32 | RoleFixture::String | RoleFixture::Bool | RoleFixture::F64 => {
                    java_sum_type(&["Shapes.Unit", &value])
                }
                other => unreachable!("unknown protocol scalar projection fixture `{other:?}`"),
            };
            m(vec![java_host_ref_type(scalar, host_types)], ret)
        }
        HostFnBodyKind::ScalarIsTrue { scalar, bool_ } => {
            m(vec![java_host_ref_type(scalar, host_types)], role(bool_))
        }
        HostFnBodyKind::MakeToken { .. }
        | HostFnBodyKind::TokenValue { .. }
        | HostFnBodyKind::BoxGet { .. }
        | HostFnBodyKind::BoxMake { .. }
        | HostFnBodyKind::CallStep { .. }
        | HostFnBodyKind::MakePairCallback { .. }
        | HostFnBodyKind::MakeStep { .. }
        | HostFnBodyKind::ApplyPoly { .. }
        | HostFnBodyKind::MakePairStructural { .. }
        | HostFnBodyKind::ProducePair { .. }
        | HostFnBodyKind::SumToString { .. }
        | HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot
        | HostFnBodyKind::StagedSecond { .. }
        | HostFnBodyKind::NestedCurriedRoundtrip { .. }
        | HostFnBodyKind::InvokeSubstitutedUnitCallback { .. }
        | HostFnBodyKind::ReturnedForallUnit
        | HostFnBodyKind::ObservePacked { .. }
        | HostFnBodyKind::TraceUnit { .. }
        | HostFnBodyKind::StagedUnitCall
        | HostFnBodyKind::UnreachableI32Print { .. } => {
            unreachable!(
                "bespoke Java host body `{}` reached canonical signature rendering",
                binding.leaf
            )
        }
    }
}

/// One `array_*` host fn's Java-typed method. The array slots use the exact
/// declaration-owned `KioHostType_<declaration><T>` carrier and retain the
/// element as a method type parameter; only the adapter's private body side
/// is `Object`.
fn java_array_method(
    leaf: &str,
    operation: &str,
    array: protocol::HostTypeIdentity,
    index: Option<HostRoleRef>,
    host_types: &[HostTypeBinding],
    member: &dyn Fn(&str) -> String,
) -> TraitMethod {
    let m = |args: Vec<String>, ret: String| TraitMethod {
        name: member(leaf),
        type_params: vec!["KioCallType_0".to_owned()],
        arg_types: args,
        ret_type: ret,
        where_clause: String::new(),
    };
    let arr = format!(
        "{}<KioCallType_0>",
        java_host_carrier(java_host_binding(array, host_types))
    );
    let index = index
        .map(|role| java_role_ref_type(role, host_types).to_owned())
        .unwrap_or_else(|| "Integer".to_owned());
    let element = "KioCallType_0".to_owned();
    let unit = "Shapes.Unit".to_owned();
    match operation {
        "make-empty" => m(vec![], arr),
        "make-filled" => m(vec![index, element], arr),
        "len" => m(vec![arr], index),
        "get" => m(vec![arr, index], element),
        "set" => m(vec![arr, index, element], "()".to_owned()),
        "push" => m(vec![arr, element], "()".to_owned()),
        "pop-back" => m(vec![arr], java_sum_type(&[&element, &unit])),
        "swap" => m(vec![arr, index.clone(), index], "()".to_owned()),
        "clear" => m(vec![arr], "()".to_owned()),
        "clone" => m(vec![arr.clone()], arr),
        other => unreachable!("unknown protocol array operation `{other}`"),
    }
}

// =========================================================================
// Driver synthesis.
// =========================================================================

/// Build the Java `Driver.java`: a `StubHost` implementing the emitted
/// `<Handle>Host` interface (each method a canonical body) plus a `main`
/// that instantiates the package and runs the protocol.
fn build_driver_source(
    host: &HostApi,
    protocol: RunnerProtocol,
    artifact: &EmittedPackage,
) -> String {
    let contract = protocol.contract();
    if contract.execution == ProtocolExecution::CompileOnly {
        return "public final class Driver {\n  public static void main(String[] args) {}\n}\n"
            .to_owned();
    }
    let ns = &artifact.namespace;
    let handle = &artifact.handle;
    let root_arguments = java_root_binding_arguments(contract);
    let root_use = if root_arguments.is_empty() {
        String::new()
    } else {
        format!("<{}>", root_arguments.join(", "))
    };
    let mut out = String::new();
    out.push_str("// Generated by kio-test-runner-java - do not edit by hand.\n");
    out.push_str(&format!("import {ns}.{handle};\n"));
    out.push_str(&format!("import {ns}.{handle}Host;\n"));
    out.push_str(&format!("import {ns}.Shapes;\n\n"));

    let needs_stdin = host
        .methods()
        .zip(contract.host_fns)
        .any(|(_, binding)| matches!(binding.body, HostFnBodyKind::ReadAsciiLine { .. }));
    let needs_float = contract.host_fns.iter().any(|binding| {
        matches!(
            binding.body,
            HostFnBodyKind::NumericToString {
                value: HostRoleRef {
                    fixture: RoleFixture::F32 | RoleFixture::F64,
                    ..
                },
                ..
            }
        )
    });
    let needs_wrap = contract.host_fns.iter().any(|binding| {
        matches!(
            binding.body,
            HostFnBodyKind::Arithmetic {
                number: HostRoleRef {
                    fixture: RoleFixture::I128 | RoleFixture::U128 | RoleFixture::U64,
                    ..
                },
                ..
            }
        )
    });
    let needs_returned_forall = contract
        .host_fns
        .iter()
        .any(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit));

    out.push_str("record TokenValue(int value) {}\n");
    for binding in contract.host_types {
        if let HostTypeFixture::SelectedRole(role) = binding.fixture {
            out.push_str(&format!(
                "record {}({} value) {{}}\n",
                java_selected_role_type(binding),
                java_role_type(role)
            ));
        }
    }
    for binding in contract.host_types {
        match binding.fixture {
            HostTypeFixture::Array => out.push_str(&format!(
                "record KioRunnerArray<T>(java.util.ArrayList<T> value) implements {}<T> {{}}\n",
                java_host_carrier(binding)
            )),
            HostTypeFixture::Box => out.push_str(&format!(
                "record KioRunnerBox<T>(T value) implements {}<T> {{}}\n",
                java_host_carrier(binding)
            )),
            _ => {}
        }
    }
    out.push('\n');
    out.push_str(&format!(
        "final class StubHost implements {handle}Host{root_use} {{\n"
    ));
    if needs_returned_forall {
        out.push_str("  boolean returnedForallProduced;\n\n");
    }
    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        let packed = java_newtype_type("testapi/types", "Packed", contract.host_types, &[]);
        out.push_str(&format!(
            "  java.util.function.Function<{packed}, Integer> observePacked;\n  int observations;\n"
        ));
    }
    for binding in contract.host_types {
        out.push_str(&render_host_binding_adapters(binding));
        out.push('\n');
    }
    for (m, binding) in host.methods().zip(contract.host_fns) {
        let kind = canonical_kind(binding.body);
        out.push_str(&render_method(m, &kind, binding, contract.host_types));
        out.push('\n');
    }
    out.push_str("}\n\n");

    out.push_str("public final class Driver {\n");
    if needs_stdin {
        out.push_str(
            "  static final java.io.BufferedReader STDIN =\n      new java.io.BufferedReader(new java.io.InputStreamReader(System.in));\n\n  static String readLine() {\n    try {\n      return STDIN.readLine();\n    } catch (java.io.IOException e) {\n      throw new RuntimeException(e);\n    }\n  }\n\n",
        );
    }
    if needs_float {
        out.push_str(
            "  static String floatToString(double value) {\n    if (value == Math.rint(value) && !Double.isInfinite(value)) {\n      return Long.toString((long) value);\n    }\n    return Double.toString(value);\n  }\n\n",
        );
    }
    if needs_wrap {
        out.push_str(
            "  static java.math.BigInteger wrapInt(java.math.BigInteger value, int bits, boolean signed) {\n    java.math.BigInteger modulus = java.math.BigInteger.ONE.shiftLeft(bits);\n    value = value.mod(modulus);\n    if (signed && value.compareTo(java.math.BigInteger.ONE.shiftLeft(bits - 1)) >= 0) {\n      value = value.subtract(modulus);\n    }\n    return value;\n  }\n\n  static java.math.BigInteger truncDiv(java.math.BigInteger a, java.math.BigInteger b) {\n    java.math.BigInteger q = a.abs().divide(b.abs());\n    return a.signum() != b.signum() ? q.negate() : q;\n  }\n\n  static java.math.BigInteger truncMod(java.math.BigInteger a, java.math.BigInteger b) {\n    return a.subtract(truncDiv(a, b).multiply(b));\n  }\n\n",
        );
    }
    out.push_str("  public static void main(String[] args) {\n");
    if protocol == RunnerProtocol::HostExistentialRoundtrip {
        out.push_str(&format!(
            "    var host = new StubHost();\n    {handle}{root_use} pkg = {handle}.create(host);\n"
        ));
    } else {
        out.push_str(&format!(
            "    {handle}{root_use} pkg = {handle}.create(new StubHost());\n"
        ));
    }
    out.push_str(&render_main_body(contract));
    out.push_str("  }\n}\n");
    out
}

fn java_root_binding_arguments(contract: protocol::ProtocolContract) -> Vec<String> {
    let mut bindings = contract
        .host_types
        .iter()
        .filter(|binding| binding.type_arity == 0)
        .collect::<Vec<_>>();
    bindings.sort_by_key(|binding| (binding.module, binding.leaf));
    bindings
        .into_iter()
        .map(java_nullary_host_selection)
        .collect()
}

fn render_host_binding_adapters(binding: &HostTypeBinding) -> String {
    let identity = java_host_type_identity(binding.module, binding.leaf);
    let from_body = format!("KioHostBinding_{identity}_fromBody");
    let to_body = format!("KioHostBinding_{identity}_toBody");
    let parameters = (0..binding.type_arity)
        .map(|index| format!("T{index}"))
        .collect::<Vec<_>>();
    let method_generics = if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}> ", parameters.join(", "))
    };
    let public = if parameters.is_empty() {
        java_nullary_host_selection(binding)
    } else {
        format!("{}<{}>", java_host_carrier(binding), parameters.join(", "))
    };
    let body = match binding.fixture {
        HostTypeFixture::Role(role) | HostTypeFixture::SelectedRole(role) => java_role_type(role),
        HostTypeFixture::Array
        | HostTypeFixture::Box
        | HostTypeFixture::Token
        | HostTypeFixture::Scalar => "Object",
    };
    let from_expression = match binding.fixture {
        HostTypeFixture::Role(_) => "value".to_owned(),
        HostTypeFixture::SelectedRole(_) => {
            format!("new {}(value)", java_selected_role_type(binding))
        }
        HostTypeFixture::Token => "(TokenValue) value".to_owned(),
        HostTypeFixture::Scalar => "(java.util.Map<String, Object>) value".to_owned(),
        HostTypeFixture::Array => {
            "new KioRunnerArray<>((java.util.ArrayList<T0>) value)".to_owned()
        }
        HostTypeFixture::Box => "new KioRunnerBox<>((T0) value)".to_owned(),
    };
    let to_expression = match binding.fixture {
        HostTypeFixture::Role(_) => "value".to_owned(),
        HostTypeFixture::SelectedRole(_) => "value.value()".to_owned(),
        HostTypeFixture::Token | HostTypeFixture::Scalar => "value".to_owned(),
        HostTypeFixture::Array => "((KioRunnerArray<T0>) value).value()".to_owned(),
        HostTypeFixture::Box => "((KioRunnerBox<T0>) value).value()".to_owned(),
    };
    format!(
        "  @Override\n  @SuppressWarnings(\"unchecked\")\n  public {method_generics}{public} {from_body}({body} value) {{\n    return {from_expression};\n  }}\n\n  @Override\n  @SuppressWarnings(\"unchecked\")\n  public {method_generics}{body} {to_body}({public} value) {{\n    return {to_expression};\n  }}\n"
    )
}

/// Render the `main` body that invokes the package for `protocol`.
fn render_main_body(contract: protocol::ProtocolContract) -> String {
    let export_root = contract.testapi_conformed.then_some("testapi");
    let root_ns = match export_root {
        Some(root) => format!("pkg.{}", facade_module_selector(root, true)),
        None => "pkg".to_owned(),
    };
    match contract.execution {
        ProtocolExecution::CompileOnly | ProtocolExecution::ConstructOnly => String::new(),
        ProtocolExecution::Invoke(ExportDriver::Main { module }) => java_main_call(module),
        ProtocolExecution::Invoke(ExportDriver::Coexist) => {
            unreachable!("--protocol coexist dispatches through run_coexist")
        }
        ProtocolExecution::Invoke(driver) => {
            render_export_driver(driver, &root_ns, export_root.is_some(), contract.host_types)
                .unwrap_or_else(|| unreachable!("non-main Java export driver has no renderer"))
        }
    }
}

fn java_main_call(module: &str) -> String {
    let path = module
        .split('/')
        .enumerate()
        .map(|(index, segment)| facade_module_selector(segment, index == 0))
        .collect::<Vec<_>>()
        .join(".");
    format!("    pkg.{path}.main();\n")
}

/// The export-surface roundtrip drivers: typed calls through the
/// namespace fields, printing results to match each golden's
/// `expected.stdout`. Mirrors the Go runner's drivers.
fn render_export_driver(
    driver: ExportDriver,
    root_ns: &str,
    has_export_root: bool,
    host_types: &[HostTypeBinding],
) -> Option<String> {
    let module = |source| {
        format!(
            "{root_ns}.{}",
            facade_module_selector(source, !has_export_root)
        )
    };
    let api_ns = module("api");
    let main_ns = module("main");
    let utils_ns = module("utils");
    let types_ns = module("types");
    let left_ns = module("left");
    let right_ns = module("right");
    let constructor_only = facade_type_selector("Constructor_only");
    let projector_only = facade_type_selector("Projector_only");
    let both_public = facade_type_selector("Both_public");
    let opaque_a = facade_type_selector("Opaque_a");
    let opaque_b = facade_type_selector("Opaque_b");
    let shared = facade_type_selector("Shared");
    let constructor_pair = facade_type_selector("Constructor_pair");
    let projector_pair = facade_type_selector("Projector_pair");
    let constructor_generic = facade_type_selector("Constructor_generic");
    let projector_generic = facade_type_selector("Projector_generic");
    let packed_function = facade_type_selector("Packed_function");
    let existential_unit = facade_type_selector("Existential_unit");
    let existential_empty = facade_type_selector("Existential_empty");
    let recursive_both = facade_type_selector("Recursive_both");
    let recursive_constructor = facade_type_selector("Recursive_constructor");
    let recursive_projector = facade_type_selector("Recursive_projector");
    let constructor_spread = facade_type_selector("Constructor_spread");
    let projector_spread = facade_type_selector("Projector_spread");
    let existential_spread = facade_type_selector("Existential_spread");
    let recursive_existential_function = facade_type_selector("Recursive_existential_function");
    let recursive_existential_type = java_newtype_type(
        "testapi/types",
        "Recursive_existential_function",
        host_types,
        &[],
    );
    let pair = facade_type_selector("Pair");
    let root = facade_type_selector("Root");
    let a = facade_type_selector("A");
    let b = facade_type_selector("B");
    let selected_i32 = host_types
        .iter()
        .find(|binding| {
            binding.module == "testapi"
                && binding.leaf == "I32"
                && matches!(
                    binding.fixture,
                    HostTypeFixture::SelectedRole(RoleFixture::I32)
                )
        })
        .map(java_selected_role_type)
        .unwrap_or_else(|| "Integer".to_owned());

    let body = match driver {
        ExportDriver::RustCallbackAliases => "    throw new IllegalArgumentException(\"rust-callback-aliases tests the Rust public naming contract only\");\n".to_owned(),
        ExportDriver::ModuleRoundtrip => format!(
            "    System.out.println({api_ns}.tag());\n    System.out.println({api_ns}.value());\n    System.out.println({api_ns}.echo(\"module-echo\"));\n"
        ),
        ExportDriver::NamespaceRoundtrip => format!(
            "    System.out.println({main_ns}.answer());\n    System.out.println({utils_ns}.echo(\"namespace-utils\"));\n"
        ),
        ExportDriver::PolyRoundtrip => format!(
            "    System.out.println({root_ns}.polyEcho(\"poly-string\"));\n    System.out.println({root_ns}.polyEcho(java.math.BigInteger.valueOf(42)));\n    System.out.println({root_ns}.keepLeft(\"left\", java.math.BigInteger.valueOf(99)));\n"
        ),
        ExportDriver::CallbackRoundtrip => format!(
            "    System.out.println({main_ns}.applyTwice(n -> n + 3, 10));\n    var step = {main_ns}.makeStep(4);\n    System.out.println(step.apply(5));\n"
        ),
        ExportDriver::PositionalProductRoundtrip => format!(
            "    var q = {main_ns}.makePair(7, \"hello\");\n    System.out.println(q._0() + \" \" + q._1());\n"
        ),
        ExportDriver::MultilabelRoundtrip => {
            format!(
                "    {main_ns}.say({main_ns}.{a}.mk(42), {main_ns}.{b}.mk(\"shown\\n\"));\n\
                 \x20   var row = {main_ns}.echoPair({main_ns}.{a}.mk(88), {main_ns}.{b}.mk(\"99\"));\n\
                 \x20   System.out.println({main_ns}.{a}.get(row.A()));\n\
                 \x20   System.out.println({main_ns}.{b}.get(row.B()));\n\
                 \x20   System.out.println({main_ns}.{a}.get({main_ns}.echoA({main_ns}.{a}.mk(111))));\n"
            )
        }
        ExportDriver::HostExistentialRoundtrip => {
            let packed = facade_type_selector("Packed");
            format!(r#"    int[] openings = {{0}};
    host.observePacked = value -> {types_ns}.{packed}.readPacked(value, new Shapes.KioForall_Hbcaace3ea220c8d6<Integer, Integer>() {{
      @Override public <U> Integer apply(U seed, Shapes.Fn1<U, Integer> step) {{
        openings[0]++;
        return step.apply(seed);
      }}
    }});
    var result = {main_ns}.exercise();
    if (result._0() != 37 || result._1() != 83 || host.observations != 2 || openings[0] != 2) throw new AssertionError("existential host observations changed");
    System.out.println("existential host opening ok");
"#)
        }
        ExportDriver::FunctorDictRoundtrip => {
            let box_handle = format!("{types_ns}.{}", facade_type_selector("Box"));
            let functor_handle = format!("{types_ns}.{}", facade_type_selector("Functor"));
            let box_marker = java_newtype_constructor_marker("testapi/types", "Box", host_types);
            let constructor_arguments = format!("Integer, String, {box_marker}");
            format!(
                r#"    var integers = new java.util.ArrayList<Integer>();
    var texts = new java.util.ArrayList<String>();
    Shapes.Fn1<Integer, String> toText = value -> {{ integers.add(value); return "v:" + value; }};
    Shapes.Fn1<String, Integer> toInteger = value -> {{ texts.add(value); return value.length(); }};
    var dict = {main_ns}.echoFunctor({main_ns}.boxFunctor());
    var first = {main_ns}.applyFunctor(dict, toText, {box_handle}.mkBox(42, new Shapes.Unit()));
    if (!{box_handle}.unBox(first)._0().equals("v:42")) throw new AssertionError("functor integer input");
    var second = {main_ns}.applyFunctor(dict, toInteger, {box_handle}.mkBox("apple", new Shapes.Unit()));
    if ({box_handle}.unBox(second)._0() != 5) throw new AssertionError("functor text input");
    var map = {functor_handle}.fmap(dict);
    var third = map.<Integer, String>apply(toText, pkg.KioNewtypeApplication_testapi_stypes__Box_lift({box_handle}.mkBox(7, new Shapes.Unit())));
    if (!{box_handle}.unBox(pkg.KioNewtypeApplication_testapi_stypes__Box_project(third))._0().equals("v:7")) throw new AssertionError("projected functor integer input");
    var fourth = map.<String, Integer>apply(toInteger, pkg.KioNewtypeApplication_testapi_stypes__Box_lift({box_handle}.mkBox("pear", new Shapes.Unit())));
    if ({box_handle}.unBox(pkg.KioNewtypeApplication_testapi_stypes__Box_project(fourth))._0() != 4) throw new AssertionError("projected functor text input");
    var constructorMap = new Shapes.KioForall_H8a282f737720a2df<{constructor_arguments}>() {{
      @Override public <A, B> Shapes.Apply1<{box_marker}, B> apply(Shapes.Fn1<A, B> step, Shapes.Apply1<{box_marker}, A> value) {{
        return map.<A, B>apply(step, value);
      }}
    }};
    var reconstructed = {functor_handle}.mkFunctor(constructorMap);
    var fifth = {main_ns}.applyFunctor(reconstructed, toText, {box_handle}.mkBox(19, new Shapes.Unit()));
    if (!{box_handle}.unBox(fifth)._0().equals("v:19")) throw new AssertionError("reconstructed functor integer input");
    if (!integers.equals(java.util.List.of(42, 7, 19)) || !texts.equals(java.util.List.of("apple", "pear"))) throw new AssertionError("functor callback events");
    System.out.println("functor dictionary ok");
"#
            )
        }
        ExportDriver::CallableSlotsRoundtrip => format!(
            "    int[] calls = {{0, 0}};\n\
             \x20   Shapes.Fn1<Integer, Integer> productStep = value -> {{ calls[0]++; return value + 5; }};\n\
             \x20   if ({main_ns}.applyProduct(productStep, 11) != 16) throw new AssertionError(\"product callback input\");\n\
             \x20   var echoedProduct = {main_ns}.echoProduct(productStep, 17);\n\
             \x20   if (echoedProduct._1() != 17 || echoedProduct._0().apply(echoedProduct._1()) != 22) throw new AssertionError(\"product callback roundtrip\");\n\
             \x20   var madeProduct = {main_ns}.makeProduct(23);\n\
             \x20   if (madeProduct._1() != 23 || madeProduct._0().apply(29) != 29) throw new AssertionError(\"package product callback\");\n\
             \x20   Shapes.Fn1<Integer, Integer> sumStep = value -> {{ calls[1]++; return value + 7; }};\n\
             \x20   Shapes.Sum<Shapes.Fn1<Integer, Integer>, Integer> sum = new Shapes.Sum__0<>(sumStep);\n\
             \x20   if ({main_ns}.applySum(sum, 31) != 38) throw new AssertionError(\"sum callback input\");\n\
             \x20   int echoedResult = {main_ns}.echoSum(sum).KioMatch(step -> step.apply(37), value -> {{ throw new AssertionError(\"callable sum arm changed\"); }});\n\
             \x20   if (echoedResult != 44) throw new AssertionError(\"sum callback roundtrip\");\n\
             \x20   int madeResult = {main_ns}.makeCallableSum().KioMatch(step -> step.apply(41), value -> {{ throw new AssertionError(\"callable sum arm changed\"); }});\n\
             \x20   if (madeResult != 41) throw new AssertionError(\"package sum callback\");\n\
             \x20   var scalar = {main_ns}.makeScalarSum(97);\n\
             \x20   int scalarValue = scalar.KioMatch(step -> {{ throw new AssertionError(\"scalar sum arm changed\"); }}, value -> value);\n\
             \x20   if (scalarValue != 97 || {main_ns}.applySum(scalar, 43) != 97) throw new AssertionError(\"scalar sum input\");\n\
             \x20   int echoedScalar = {main_ns}.echoSum(scalar).KioMatch(step -> {{ throw new AssertionError(\"scalar sum arm changed\"); }}, value -> value);\n\
             \x20   if (echoedScalar != 97 || calls[0] != 2 || calls[1] != 2) throw new AssertionError(\"callable observations changed\");\n\
             \x20   System.out.println(\"callable slots ok\");\n"
        ),
        ExportDriver::ScalarRoundtrip => format!(
            "    var signed = new java.math.BigInteger(\"-1208925819614629174706299\");\n\
             \x20   var unsigned = new java.math.BigInteger(\"2417851639229258349412391\");\n\
             \x20   if (!{main_ns}.echoI128(signed).equals(signed) || !{main_ns}.echoU128(unsigned).equals(unsigned)) throw new AssertionError(\"wide integer payload changed\");\n\
             \x20   for (float value : new float[] {{1.5f, -2.25f}}) if ({main_ns}.echoF32(value) != value) throw new AssertionError(\"F32 payload changed\");\n\
             \x20   for (double value : new double[] {{1.0000000000000002, -3.125}}) if ({main_ns}.echoF64(value) != value) throw new AssertionError(\"F64 payload changed\");\n\
             \x20   System.out.println(\"scalar payloads ok\");\n"
        ),
        ExportDriver::HostOwnedRoundtrip => format!(
            "    for (int value : new int[] {{7, 19}}) if ({main_ns}.echoToken(new TokenValue(value)).value() != value) throw new AssertionError(\"token payload changed\");\n\
             \x20   var integer = {main_ns}.<Integer>echoBox(new KioRunnerBox<Integer>(42));\n\
             \x20   if (((KioRunnerBox<Integer>) integer).value() != 42) throw new AssertionError(\"integer box payload changed\");\n\
             \x20   var text = {main_ns}.<String>echoBox(new KioRunnerBox<String>(\"box-value\"));\n\
             \x20   if (!((KioRunnerBox<String>) text).value().equals(\"box-value\")) throw new AssertionError(\"string box payload changed\");\n\
             \x20   System.out.println(\"host-owned payloads ok\");\n"
        ),
        ExportDriver::StructuralRoundtrip => {
            let wide_sum = java_positional_shell("Sum", 10);
            let sum_type = format!(
                "Shapes.{wide_sum}<Byte, Short, Integer, Long, Short, Integer, Long, java.math.BigInteger, Boolean, String>"
            );
            let samples = [
                (0, "(byte) -101"),
                (1, "(short) -12345"),
                (2, "-123456789"),
                (3, "-9007199254740993L"),
                (4, "(short) 201"),
                (5, "54321"),
                (6, "3456789012L"),
                (7, "new java.math.BigInteger(\"18014398509481987\")"),
                (8, "false"),
                (8, "true"),
                (9, "\"sum-value\""),
            ]
            .into_iter()
            .map(|(arm, value)| format!("new Shapes.{wide_sum}__{arm}<>({value})"))
            .collect::<Vec<_>>()
            .join(", ");
            let roundtrip = format!(
                r#"    for (var sample : java.util.List.<{sum_type}>of({samples})) {{
      var returned = {main_ns}.echoSum(sample);
      String payload = returned.KioMatch(
        value -> value.toString(), value -> value.toString(),
        value -> value.toString(), value -> value.toString(),
        value -> value.toString(), value -> value.toString(),
        value -> value.toString(), value -> value.toString(),
        value -> value.toString(), value -> value);
      System.out.println({main_ns}.classify(returned) + " " + payload);
    }}
"#
            );
            let script = format!(
                "    var q = {main_ns}.pairSwap(42, \"hello\");\n    System.out.println(q._0() + \" \" + q._1());\n    System.out.println({main_ns}.dispatchLeft(new Shapes.Sum__0<Integer, String>(7)));\n    System.out.println({main_ns}.dispatchLeft(new Shapes.Sum__1<Integer, String>(\"from-sum\")));\n    var rotated = {main_ns}.rotate(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);\n    System.out.println(rotated._0() + \" \" + rotated._1() + \" \" + rotated._11());\n    System.out.println({main_ns}.classify(new Shapes.{wide_sum}__0<Byte, Short, Integer, Long, Short, Integer, Long, java.math.BigInteger, Boolean, String>((byte) 1)));\n    System.out.println({main_ns}.classify(new Shapes.{wide_sum}__4<Byte, Short, Integer, Long, Short, Integer, Long, java.math.BigInteger, Boolean, String>((short) 5)));\n    System.out.println({main_ns}.classify(new Shapes.{wide_sum}__9<Byte, Short, Integer, Long, Short, Integer, Long, java.math.BigInteger, Boolean, String>(\"ten\")));\n    System.out.println({main_ns}.classify({main_ns}.chooseFirst()));\n    System.out.println({main_ns}.classify({main_ns}.chooseMiddle()));\n    System.out.println({main_ns}.classify({main_ns}.chooseLast()));\n"
            );
            script + &roundtrip
        }
        ExportDriver::NewtypeSumRoundtrip => {
            format!("    System.out.println({main_ns}.firstOr(0, {main_ns}.pack(7, \"hi\")));\n")
        }
        ExportDriver::CurriedFacade => format!(
            "    System.out.println({main_ns}.pick(\"ku\", \"rz\"));\n    System.out.println({main_ns}.last(1, 2, 3));\n"
        ),
        ExportDriver::NestedCurriedRoundtrip => {
            let inner = java_function_type(&["String"], "String");
            let callback = java_function_type(&["String"], &inner);
            format!(
                "    {callback} join = left -> right -> left + \"/\" + right;\n\
                     {callback} viaHost = {api_ns}.viaHost(join);\n\
                     System.out.println(\"via host: \" + viaHost.apply(\"env-left\").apply(\"env-right\"));\n\
                     {callback} roundExport = {api_ns}.roundExport(join);\n\
                     System.out.println(\"round export: \" + roundExport.apply(\"export-left\").apply(\"export-right\"));\n"
            )
        }
        ExportDriver::HostSubstitutedUnitCallback => {
            format!("    System.out.println({api_ns}.viaHost(_unit -> \"callback\"));\n")
        }
        ExportDriver::ReturnedForallCallByValue => format!(
            "    {main_ns}.main();\n\
                 try {{\n\
                   {main_ns}.main();\n\
                   throw new AssertionError(\"produce did not throw\");\n\
                 }} catch (IllegalStateException error) {{\n\
                   if (!\"produce failed\".equals(error.getMessage())) throw error;\n\
                   System.out.println(\"caught\");\n\
                 }}\n"
        ),
        ExportDriver::FacadeSelectorCollisions => {
            let child_module = facade_module_selector("child", false);
            let child_type = facade_type_selector("Child");
            let foo_module = module("foo");
            let foo_bar = module("foo_bar");
            let i = module("i");
            let host = module("host");
            let mod_api_value = module("mod_api_value");
            let bar = facade_module_selector("bar", false);
            format!(
                "    {api_ns}.pkg();\n\
                     {api_ns}.value();\n\
                     {host}.value();\n\
                     {mod_api_value}.value();\n\
                     System.out.println({foo_module}.{bar}.value(9, 1));\n\
                     System.out.println({foo_bar}.value(10, 2));\n\
                     System.out.println({i}.value(41, 1));\n\
                     {api_ns}.child();\n\
                     System.out.println(\"api.child function\");\n\
                     {api_ns}.{child_module}.value();\n\
                     System.out.println(\"api/child module\");\n\
                     var child = {api_ns}.{child_type}.makeChild(30);\n\
                     {api_ns}.{child_type}.readChild(child);\n\
                     System.out.println(\"api.Child type\");\n"
            )
        }
        ExportDriver::ModuleAliasScopeCollision => {
            let a = module("a");
            let b = module("b");
            format!(
                "    var aValue = {a}.make();\n\
                     {a}.consume(aValue._0(), aValue._1());\n\
                     var bValue = {b}.make();\n\
                     {b}.consume(bValue._0(), bValue._1(), bValue._2());\n"
            )
        }
        ExportDriver::WideCallable => {
            let shell = java_positional_shell("Product", WIDE_CALLABLE_SLOT_COUNT);
            let types = (0..WIDE_CALLABLE_SLOT_COUNT)
                .map(|_| "Integer")
                .collect::<Vec<_>>()
                .join(", ");
            let setters = (0..WIDE_CALLABLE_SLOT_COUNT)
                .map(|index| format!("._{index}({index})"))
                .collect::<String>();
            format!(
                "    var wideArgs = Shapes.{shell}.<{types}>builder(){setters}.build();\n    var out = {main_ns}.select(wideArgs);\n    System.out.println(out._0());\n    System.out.println(out._1());\n    System.out.println(out._2());\n    var callbackOut = {main_ns}.makeSelect().apply(wideArgs);\n    System.out.println(callbackOut._0());\n    System.out.println(callbackOut._1());\n    System.out.println(callbackOut._2());\n"
            )
        }
        // `apply_via[K][R](f: K -> R, x: K) -> R` retains `K` and `R` as
        // method parameters and uses the generic `Shapes.Fn1<K, R>` SAM.
        // Each call independently selects its concrete instantiation.
        ExportDriver::PolyCallbackRoundtrip => format!(
            "    System.out.println((String) {main_ns}.applyVia(v -> \"via: \" + (String) v, \"apply\"));\n    System.out.println((Integer) {main_ns}.applyVia(v -> (Integer) v + 8, 7));\n"
        ),
        ExportDriver::NewtypeScalarRoundtrip => format!(
            "    var wrapped = {main_ns}.{wrap}.mkWrap(7);\n    var bumped = {main_ns}.bump(wrapped);\n    System.out.println(bumped.value());\n",
            wrap = facade_type_selector("Wrap"),
        ),
        ExportDriver::NewtypeIgnoredArgumentRoundtrip => {
            format!("    System.out.println({main_ns}.toI32({main_ns}.fromI32(7)));\n")
        }
        ExportDriver::RecursiveNewtypeBoundary => format!(
            "    var payload = {main_ns}.basePayload();\n\
             \x20   var root = {main_ns}.{root}.makeRoot(payload);\n\
             \x20   var kept = {main_ns}.keep(root);\n\
             \x20   var projected = {main_ns}.{root}.readRoot(kept);\n\
             \x20   System.out.println({main_ns}.acceptPayload(projected));\n"
        ),
        ExportDriver::NewtypeVisibilityFacade => format!(
            "    var a = {types_ns}.makeA(new {selected_i32}(11));\n\
             \x20   System.out.println({types_ns}.readA(a).value());\n\
             \x20   var b = {types_ns}.makeB(new {selected_i32}(22));\n\
             \x20   System.out.println({types_ns}.readB(b).value());\n\
             \x20   var c = {types_ns}.{constructor_only}.makeConstructorOnly(new {selected_i32}(33));\n\
             \x20   System.out.println({types_ns}.readConstructorOnlyValue(c).value());\n\
             \x20   var p = {types_ns}.makeProjectorOnlyValue(new {selected_i32}(44));\n\
             \x20   System.out.println({types_ns}.{projector_only}.readProjectorOnly(p).value());\n\
             \x20   var d = {types_ns}.{both_public}.makeBothPublic(new {selected_i32}(55));\n\
             \x20   System.out.println({types_ns}.{both_public}.readBothPublic(d).value());\n\
             \x20   if ({types_ns}.{opaque_a} == null || {types_ns}.{opaque_b} == null) throw new AssertionError(\"opaque newtype handles must be nameable\");\n\
             \x20   var leftShared = {left_ns}.make(new {selected_i32}(66));\n\
             \x20   var outL = {left_ns}.{shared}.readShared({left_ns}.{shared}.makeShared({left_ns}.read(leftShared)));\n\
             \x20   System.out.println(outL.value());\n\
             \x20   var rightShared = {right_ns}.make(new {selected_i32}(77));\n\
             \x20   var outR = {right_ns}.{shared}.readShared({right_ns}.{shared}.makeShared({right_ns}.read(rightShared)));\n\
             \x20   System.out.println(outR.value());\n\
             \x20   var cp = {types_ns}.{constructor_pair}.makeConstructorPair(new {selected_i32}(81), new {selected_i32}(82));\n\
             \x20   var cpOut = {types_ns}.readConstructorPairValue(cp);\n\
             \x20   System.out.println(cpOut._0().value() + \" \" + cpOut._1().value());\n\
             \x20   var pp = {types_ns}.makeProjectorPairValue(cpOut._0(), cpOut._1());\n\
             \x20   var ppOut = {types_ns}.{projector_pair}.readProjectorPair(pp);\n\
             \x20   System.out.println(ppOut._0().value() + \" \" + ppOut._1().value());\n\
             \x20   var cg = {types_ns}.{constructor_generic}.makeConstructorGeneric(85);\n\
             \x20   System.out.println({types_ns}.readConstructorGenericValue(cg));\n\
             \x20   var pg = {types_ns}.makeProjectorGenericValue(86);\n\
             \x20   System.out.println({types_ns}.{projector_generic}.readProjectorGeneric(pg));\n\
             \x20   var packed = {types_ns}.{packed_function}.makePackedFunction((left, right) -> new {selected_i32}(left.value() + right.value()));\n\
             \x20   System.out.println({types_ns}.{packed_function}.readPackedFunction(packed).apply(cpOut._0(), cpOut._1()).value());\n\
             \x20   var existentialUnit = {types_ns}.makeExistentialUnitValue();\n\
             \x20   // Java lambdas cannot implement these generic SAM methods.\n\
             \x20   var openUnit = new Shapes.KioForall_H8f1ae0de6f219de2<{selected_i32}, Object>() {{\n\
             \x20     @Override public <KioPolyType_1> Object apply(KioPolyType_1 value) {{ return 89; }}\n\
             \x20   }};\n\
             \x20   Object openedUnit = {types_ns}.{existential_unit}.readExistentialUnit(existentialUnit, openUnit);\n\
             \x20   System.out.println(openedUnit);\n\
             \x20   var existentialEmpty = {types_ns}.makeExistentialEmptyValue();\n\
             \x20   var openEmpty = new Shapes.KioForall_H8b5e594f5b106bfa<{selected_i32}, Object>() {{\n\
             \x20     @Override public <KioPolyType_1> Object apply() {{ return 90; }}\n\
             \x20   }};\n\
             \x20   Object openedEmpty = {types_ns}.{existential_empty}.readExistentialEmpty(existentialEmpty, openEmpty);\n\
             \x20   System.out.println(openedEmpty);\n\
             \x20   var rbPayload = {types_ns}.recursiveBothBasePayload();\n\
             \x20   var rb = {types_ns}.{recursive_both}.makeRecursiveBoth(rbPayload);\n\
             \x20   System.out.println({types_ns}.recursiveBothPayloadIsBase({types_ns}.{recursive_both}.readRecursiveBoth(rb)).value());\n\
             \x20   var rcPayload = {types_ns}.recursiveConstructorBasePayload();\n\
             \x20   var rc = {types_ns}.{recursive_constructor}.makeRecursiveConstructor(rcPayload);\n\
             \x20   System.out.println({types_ns}.recursiveConstructorPayloadIsBase({types_ns}.readRecursiveConstructorValue(rc)).value());\n\
             \x20   var rp = {types_ns}.makeRecursiveProjectorBase();\n\
             \x20   System.out.println({types_ns}.recursiveProjectorPayloadIsBase({types_ns}.{recursive_projector}.readRecursiveProjector(rp)).value());\n\
             \x20   var inputs = new java.util.ArrayList<Integer>();\n\
             \x20   var spread = new Shapes.KioForall_H69959d5bd62d04c2<{selected_i32}>() {{\n\
             \x20     @Override public <A> {selected_i32} apply({selected_i32} seed, A other) {{ inputs.add(seed.value()); return new {selected_i32}(seed.value() + 3); }}\n\
             \x20   }};\n\
             \x20   var constructed = {types_ns}.{constructor_spread}.makeConstructorSpread(spread);\n\
             \x20   if ({types_ns}.invokeConstructorSpreadI32(constructed, new {selected_i32}(101)).value() != 104 || {types_ns}.invokeConstructorSpreadUnit(constructed, new {selected_i32}(102)).value() != 105 || !inputs.equals(java.util.List.of(101, 102))) throw new AssertionError(\"constructor callback changed\");\n\
             \x20   var projected = {types_ns}.{projector_spread}.readProjectorSpread({types_ns}.makeProjectorSpreadValue());\n\
             \x20   if (projected.<{selected_i32}>apply(new {selected_i32}(111), new {selected_i32}(1)).value() != 111 || projected.<Shapes.Unit>apply(new {selected_i32}(112), new Shapes.Unit()).value() != 112) throw new AssertionError(\"projector callback changed\");\n\
             \x20   int[] opens = {{0, 0}};\n\
             \x20   var openSpread = new Shapes.KioForall_Hd3af5fb1043c9a70<{selected_i32}, Integer>() {{\n\
             \x20     @Override public <U> Integer apply(Shapes.KioForall_H028c69c9119445cf<{selected_i32}, Integer, U> payload) {{ opens[0]++; return 91; }}\n\
             \x20   }};\n\
             \x20   var openRecursive = new Shapes.KioForall_H1ab7eb9ed8bafd42<{selected_i32}, Integer>() {{\n\
             \x20     @Override public <U> Integer apply(Shapes.Fn1<U, {recursive_existential_type}> payload) {{ opens[1]++; return 92; }}\n\
             \x20   }};\n\
             \x20   if ({types_ns}.{existential_spread}.readExistentialSpread({types_ns}.makeExistentialSpreadValue(), openSpread) != 91 || {types_ns}.{recursive_existential_function}.readRecursiveExistentialFunction({types_ns}.makeRecursiveExistentialFunctionValue(), openRecursive) != 92 || opens[0] != 1 || opens[1] != 1) throw new AssertionError(\"existential continuation count changed\");\n"
        ),
        // `make(String, I32, I32) -> Outer`, where the exact public
        // `Inner` carrier wraps `(I32 & I32)` and the exact `Outer`
        // carrier wraps `(String & Inner)`. Type inference retains both
        // carrier identities while `var` lets the driver read the
        // generated generic structural payload without duplicating it.
        ExportDriver::NestedProductRoundtrip => format!(
            "    var p = {main_ns}.make(\"nest\", 7, 9).value();\n    System.out.println(p._0());\n    var q = p.Inner().value();\n    System.out.println(q._0());\n    System.out.println(q._1());\n"
        ),
        ExportDriver::PublicWordNames => format!(
            "    System.out.println({root_ns}.wordApi.readWord());\n\
             \x20   System.out.println({root_ns}.wordApi._readWord());\n\
             \x20   System.out.println({root_ns}.wordApi.readWord_());\n\
             \x20   System.out.println({root_ns}.wordApi._readWord_());\n\
             \x20   System.out.println({root_ns}.wordApi.readWord__());\n\
             \x20   var boxed = {root_ns}.wordApi.KioModule_wordNodes.KioType__uWordBox_u_u.wrapWord(55);\n\
             \x20   System.out.println({root_ns}.wordApi.KioModule_wordNodes.KioType__uWordBox_u_u.unwrapWord(boxed));\n\
             \x20   System.out.println({root_ns}.wordApi.KioModule_wordNodes.keepWord(66));\n\
             \x20   var pair = {root_ns}.wordApi.keepPair({root_ns}.wordApi.KioModule_wordNodes.KioType__uWordBox_u_u.wrapWord(77), {root_ns}.wordApi.KioModule_otherNodes.KioType__uWordBox_u_u.wrapWord(88));\n\
             \x20   System.out.println({root_ns}.wordApi.KioModule_wordNodes.KioType__uWordBox_u_u.unwrapWord(pair._WordBox__()));\n\
             \x20   System.out.println({root_ns}.wordApi.KioModule_otherNodes.KioType__uWordBox_u_u.unwrapWord(pair.KioQualified_wordApi_sotherNodes___uWordBox_u_u()));\n"
        ),
        ExportDriver::CompoundInputOnce => format!(
            r#"    var direct = {main_ns}.direct();
    System.out.println(direct._0());
    System.out.println(direct._1());
    var callback = {main_ns}.callback(() -> {{
      System.out.println("callback");
      return new Shapes.Product<>(9, "callback-value");
    }});
    System.out.println(callback._0());
    System.out.println(callback._1());
    var outer = {main_ns}.echoOuter({main_ns}.makeOuter("nest", 11, 13)).value();
    System.out.println(outer._0());
    System.out.println(outer.Inner().value()._0());
    System.out.println(outer.Inner().value()._1());
    for (var choice : java.util.List.of({main_ns}.first(17), {main_ns}.middle(19, 23), {main_ns}.last("choice", 29, 31))) {{
      {main_ns}.echoChoice(choice).value().KioMatch(
        value -> {{ System.out.println(value); return 0; }},
        value -> {{ System.out.println(value.value()._0()); System.out.println(value.value()._1()); return 0; }},
        value -> {{
          var product = value.value();
          System.out.println(product._0());
          System.out.println(product.Inner().value()._0());
          System.out.println(product.Inner().value()._1());
          return 0;
        }});
    }}
    System.out.println({main_ns}.echoText("atomic"));
"#
        ),
        ExportDriver::TypeRoundtrip => format!(
            "    var boxed = {types_ns}.{pair}.mkPair(\"export-type-left\", \"export-type-right\");\n    var outp = {types_ns}.{pair}.unPair(boxed);\n    System.out.println(outp._0());\n    System.out.println(outp._1());\n"
        ),
        ExportDriver::Main { .. } | ExportDriver::Coexist => return None,
    };
    Some(body)
}

/// Render one `StubHost` method: the Java signature + a canonical body.
fn render_method(
    m: &TraitMethod,
    kind: &CanonicalKind,
    binding: &HostFnBinding,
    host_types: &[HostTypeBinding],
) -> String {
    let sig = render_method_sig(m);
    let body = render_java_body_for_binding(binding, kind, m, host_types);
    format!(
        "  @Override\n  @SuppressWarnings(\"unchecked\")\n  public {sig} {{\n    {body}\n  }}\n"
    )
}

fn canonical_kind(body: HostFnBodyKind) -> CanonicalKind {
    match body {
        HostFnBodyKind::Print { .. } => CanonicalKind::Print,
        HostFnBodyKind::Eprint { .. } => CanonicalKind::Eprint,
        HostFnBodyKind::Exit { .. } => CanonicalKind::Exit,
        HostFnBodyKind::ReadAsciiLine { .. } => CanonicalKind::ReadAsciiLine,
        HostFnBodyKind::StringConcat { .. } => CanonicalKind::StringConcat,
        HostFnBodyKind::StringEq { .. } => CanonicalKind::StringEq,
        HostFnBodyKind::StringLen { .. } => CanonicalKind::StringLen,
        HostFnBodyKind::StringSlice { .. } => CanonicalKind::StringSlice,
        HostFnBodyKind::StringCodeAt { .. } => CanonicalKind::StringCodeAt,
        HostFnBodyKind::Loop => CanonicalKind::Loop,
        HostFnBodyKind::NumericToString { value, .. } => CanonicalKind::NumericToString {
            kind: value.fixture.role().to_owned(),
        },
        HostFnBodyKind::BoolToString { .. } => CanonicalKind::BoolToString,
        HostFnBodyKind::PrintI32 { .. } => CanonicalKind::PrintI32,
        HostFnBodyKind::StringToInt { .. } => CanonicalKind::StringToInt,
        HostFnBodyKind::Arithmetic { operation, number } => CanonicalKind::Arith {
            op: operation.to_owned(),
            kind: number.fixture.role().to_owned(),
        },
        HostFnBodyKind::FloatArithmetic { operation, number } => CanonicalKind::FloatArith {
            op: operation.to_owned(),
            kind: number.fixture.role().to_owned(),
        },
        HostFnBodyKind::Compare {
            operation, number, ..
        } => CanonicalKind::Cmp {
            cmp: operation.to_owned(),
            kind: number.fixture.role().to_owned(),
        },
        HostFnBodyKind::Array { operation, .. } => CanonicalKind::Array(match operation {
            "make-empty" => ArrayOp::MakeEmpty,
            "make-filled" => ArrayOp::MakeFilled,
            "len" => ArrayOp::Len,
            "get" => ArrayOp::Get,
            "set" => ArrayOp::Set,
            "push" => ArrayOp::Push,
            "pop-back" => ArrayOp::PopBack,
            "swap" => ArrayOp::Swap,
            "clear" => ArrayOp::Clear,
            "clone" => ArrayOp::Clone,
            other => unreachable!("unknown protocol array operation `{other}`"),
        }),
        HostFnBodyKind::MakeScalar { .. } => CanonicalKind::MakeScalar,
        HostFnBodyKind::ScalarOf { value, .. } => CanonicalKind::ScalarOf {
            kind: value.fixture.role().to_owned(),
        },
        HostFnBodyKind::ScalarAs { value, .. } => CanonicalKind::ScalarAs {
            kind: value.fixture.role().to_owned(),
        },
        HostFnBodyKind::ScalarIsTrue { .. } => CanonicalKind::ScalarIsTrue,
        HostFnBodyKind::MakeToken { .. }
        | HostFnBodyKind::TokenValue { .. }
        | HostFnBodyKind::BoxGet { .. }
        | HostFnBodyKind::BoxMake { .. }
        | HostFnBodyKind::CallStep { .. }
        | HostFnBodyKind::MakePairCallback { .. }
        | HostFnBodyKind::MakeStep { .. }
        | HostFnBodyKind::ApplyPoly { .. }
        | HostFnBodyKind::MakePairStructural { .. }
        | HostFnBodyKind::ProducePair { .. }
        | HostFnBodyKind::SumToString { .. }
        | HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot
        | HostFnBodyKind::StagedSecond { .. }
        | HostFnBodyKind::NestedCurriedRoundtrip { .. }
        | HostFnBodyKind::InvokeSubstitutedUnitCallback { .. }
        | HostFnBodyKind::ReturnedForallUnit
        | HostFnBodyKind::ObservePacked { .. }
        | HostFnBodyKind::TraceUnit { .. }
        | HostFnBodyKind::StagedUnitCall
        | HostFnBodyKind::UnreachableI32Print { .. } => CanonicalKind::Custom,
    }
}

/// Render the Java method signature for `m`: params `p0`, `p1`, …; a
/// `()` return renders `void`. Method-local type parameters remain real
/// Java method parameters so the external host witnesses the same generic
/// relationship as the generated Host surface.
fn render_method_sig(m: &TraitMethod) -> String {
    let mut params = Vec::with_capacity(m.arg_types.len());
    for (i, ty) in m.arg_types.iter().enumerate() {
        params.push(format!("{ty} p{i}"));
    }
    let ret = m.ret_type.clone();
    let ret = if ret == "()" { "void".to_owned() } else { ret };
    let generics = if m.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}> ", m.type_params.join(", "))
    };
    format!("{generics}{ret} {}({})", m.name, params.join(", "))
}

/// The two variant records of a generic sum shell: `<shell>__0` /
/// `<shell>__1` (the positional members `_0` / `_1`).
fn sum_ctors(shell: &str) -> (String, String) {
    let base = shell.split_once('<').map_or(shell, |(base, _)| base);
    (format!("{base}__0"), format!("{base}__1"))
}

/// Render the Java body for `m`, honoring the protocol's bespoke host
/// fns before the canonical-kind dispatch. Mirrors the Go runner's
/// semantics; typed shaped slots are built / matched through the emitted
/// generic `Shapes` shells.
fn render_java_body_for_binding(
    binding: &HostFnBinding,
    kind: &CanonicalKind,
    m: &TraitMethod,
    host_types: &[HostTypeBinding],
) -> String {
    if let Some(body) = render_selected_role_body(binding.body, host_types) {
        return body;
    }
    match binding.body {
        HostFnBodyKind::CallStep { .. } => {
            "return p0.apply(p1, \"compound-callback\", true);".to_owned()
        }
        HostFnBodyKind::MakePairCallback { .. } => "return p0.apply(p1)._0();".to_owned(),
        HostFnBodyKind::MakeStep { .. } => "return n -> n + p0;".to_owned(),
        HostFnBodyKind::BoxMake { .. } => "return new KioRunnerBox<>(p0);".to_owned(),
        HostFnBodyKind::BoxGet { .. } => {
            "return ((KioRunnerBox<KioCallType_0>) p0).value();".to_owned()
        }
        HostFnBodyKind::MakeToken { .. } => "return new TokenValue(p0);".to_owned(),
        HostFnBodyKind::TokenValue { .. } => "return p0.value();".to_owned(),
        HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot => "return p0;".to_owned(),
        HostFnBodyKind::ObservePacked { .. } => "observations++; return observePacked.apply(p0);".to_owned(),
        HostFnBodyKind::StagedSecond { .. } => "return p1;".to_owned(),
        HostFnBodyKind::NestedCurriedRoundtrip { .. } => {
            "System.out.println(\"round host probe: \" + p0.apply(\"host-left\").apply(\"host-right\"));\n    return p0;".to_owned()
        }
        HostFnBodyKind::InvokeSubstitutedUnitCallback { .. } => {
            "return \"host/\" + p0.apply(new Shapes.Unit());".to_owned()
        }
        HostFnBodyKind::ReturnedForallUnit => {
            format!(
                "if (returnedForallProduced) {{\n      System.out.println(\"throw\");\n      throw new IllegalStateException(\"produce failed\");\n    }}\n    returnedForallProduced = true;\n    System.out.println(\"produce\");\n    return new {}() {{\n      @Override\n      public <KioPolyType_0> KioPolyType_0 apply() {{\n        return null;\n      }}\n    }};",
                m.ret_type
            )
        }
        HostFnBodyKind::TraceUnit { text } => format!("System.out.println({text:?});"),
        HostFnBodyKind::StagedUnitCall => {
            "System.out.println(\"staged Unit host call\");".to_owned()
        }
        HostFnBodyKind::ApplyPoly { .. } => "return p0.apply(\"rank-n\\n\");".to_owned(),
        HostFnBodyKind::MakePairStructural { .. } => {
            "return new Shapes.Product<>(p0, p1);".to_owned()
        }
        HostFnBodyKind::ProducePair { .. } => {
            "System.out.println(\"direct\");\n    return new Shapes.Product<>(7, \"direct-value\");".to_owned()
        }
        HostFnBodyKind::SumToString { .. } => {
            "return p0.KioMatch(String::valueOf, value -> value);".to_owned()
        }
        HostFnBodyKind::UnreachableI32Print { .. } => {
            "throw new RuntimeException(\"unreachable host function\");".to_owned()
        }
        _ => render_java_body(kind, m),
    }
}

fn unwrap_role_value(
    role: HostRoleRef,
    expression: &str,
    host_types: &[HostTypeBinding],
) -> String {
    match role.resolve(host_types).fixture {
        HostTypeFixture::SelectedRole(_) => format!("({expression}).value()"),
        HostTypeFixture::Role(_) => expression.to_owned(),
        other => unreachable!("Java role value resolved to non-role fixture {other:?}"),
    }
}

fn wrap_role_value(role: HostRoleRef, expression: &str, host_types: &[HostTypeBinding]) -> String {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::SelectedRole(_) => {
            format!("new {}({expression})", java_selected_role_type(binding))
        }
        HostTypeFixture::Role(_) => expression.to_owned(),
        other => unreachable!("Java role value resolved to non-role fixture {other:?}"),
    }
}

fn wrap_return_role(body: String, role: HostRoleRef, host_types: &[HostTypeBinding]) -> String {
    let Some(expression) = body
        .strip_prefix("return ")
        .and_then(|body| body.strip_suffix(';'))
    else {
        unreachable!("a selected Java direct-role body has one return expression")
    };
    format!("return {};", wrap_role_value(role, expression, host_types))
}

fn render_selected_role_body(
    body: HostFnBodyKind,
    host_types: &[HostTypeBinding],
) -> Option<String> {
    let has_selected = |role: HostRoleRef| {
        matches!(
            role.resolve(host_types).fixture,
            HostTypeFixture::SelectedRole(_)
        )
    };
    match body {
        HostFnBodyKind::Print { string } if has_selected(string) => Some(format!(
            "System.out.print({});\n    System.out.flush();",
            unwrap_role_value(string, "p0", host_types)
        )),
        HostFnBodyKind::Eprint { string } if has_selected(string) => Some(format!(
            "System.err.print({});\n    System.err.flush();",
            unwrap_role_value(string, "p0", host_types)
        )),
        HostFnBodyKind::PrintI32 { value } if has_selected(value) => Some(format!(
            "System.out.print(String.valueOf({}));\n    System.out.flush();",
            unwrap_role_value(value, "p0", host_types)
        )),
        HostFnBodyKind::NumericToString { value, string }
            if has_selected(value) || has_selected(string) =>
        {
            let body = numeric_to_string_body(value.fixture.role())
                .replace("p0", &unwrap_role_value(value, "p0", host_types));
            Some(wrap_return_role(body, string, host_types))
        }
        HostFnBodyKind::Arithmetic { operation, number } if has_selected(number) => {
            let body = int_arith_body(operation, number.fixture.role())
                .replace("p0", &unwrap_role_value(number, "p0", host_types))
                .replace("p1", &unwrap_role_value(number, "p1", host_types));
            Some(wrap_return_role(body, number, host_types))
        }
        HostFnBodyKind::MakeToken { value_i32, .. } if has_selected(value_i32) => Some(format!(
            "return new TokenValue({});",
            unwrap_role_value(value_i32, "p0", host_types)
        )),
        HostFnBodyKind::TokenValue { value_i32, .. } if has_selected(value_i32) => Some(format!(
            "return {};",
            wrap_role_value(value_i32, "p0.value()", host_types)
        )),
        _ => None,
    }
}

/// Render a Java body for one canonical kind. Semantics mirror the other
/// runners' canonical bodies (wrap-at-width integer arithmetic, the
/// shortest-integer float formatting, ASCII stdin).
fn render_java_body(kind: &CanonicalKind, m: &TraitMethod) -> String {
    match kind {
        CanonicalKind::Print => "System.out.print(p0);\n    System.out.flush();".to_owned(),
        CanonicalKind::Eprint => "System.err.print(p0);\n    System.err.flush();".to_owned(),
        CanonicalKind::Exit => {
            "System.exit((int) Math.max(0, Math.min(125, (long) p0)));\n    return null;"
                .to_owned()
        }
        CanonicalKind::ReadAsciiLine => {
            let (v0, v1) = sum_ctors(&m.ret_type);
            format!(
                "String line = Driver.readLine();\n    if (line == null) {{\n      return new {v1}<>(new Shapes.Unit());\n    }}\n    for (int i = 0; i < line.length(); i++) {{\n      if (line.charAt(i) > 0x7f) {{\n        System.err.println(\"read_ascii_line: non-ASCII input\");\n        System.exit(1);\n      }}\n    }}\n    return new {v0}<>(line);"
            )
        }
        CanonicalKind::StringLen => "return p0.length();".to_owned(),
        CanonicalKind::StringSlice => {
            "if (p1 < 0 || p1 > p2 || p2 > p0.length()) {\n      throw new RuntimeException(\"string_slice: invalid range [\" + p1 + \", \" + p2 + \") for len \" + p0.length());\n    }\n    return p0.substring(p1, p2);"
                .to_owned()
        }
        CanonicalKind::StringCodeAt => {
            let (v0, v1) = sum_ctors(&m.ret_type);
            format!(
                "if (p1 < 0 || p1 >= p0.length()) {{\n      return new {v1}<>(new Shapes.Unit());\n    }}\n    return new {v0}<>((int) p0.charAt(p1));"
            )
        }
        CanonicalKind::StringConcat => "return p0 + p1;".to_owned(),
        CanonicalKind::StringEq => "return p0.equals(p1);".to_owned(),
        CanonicalKind::StringToInt => {
            let (v0, v1) = sum_ctors(&m.ret_type);
            format!(
                "if (!p0.matches(\"^[+-]?\\\\d+$\")) {{\n      return new {v1}<>(new Shapes.Unit());\n    }}\n    java.math.BigInteger n = new java.math.BigInteger(p0);\n    if (n.bitLength() > 31) {{\n      return new {v1}<>(new Shapes.Unit());\n    }}\n    return new {v0}<>(n.intValue());"
            )
        }
        CanonicalKind::BoolToString => "return String.valueOf(p0);".to_owned(),
        CanonicalKind::PrintI32 => {
            "System.out.print(String.valueOf(p0));\n    System.out.flush();".to_owned()
        }
        CanonicalKind::NumericToString { kind } => numeric_to_string_body(kind),
        CanonicalKind::Arith { op, kind } => int_arith_body(op, kind),
        CanonicalKind::FloatArith { op, .. } => float_arith_body(op),
        CanonicalKind::Cmp { cmp, kind } => cmp_body(cmp, kind),
        CanonicalKind::Loop => render_loop_body(m),
        CanonicalKind::Array(op) => render_array_body(op, m),
        CanonicalKind::MakeScalar => {
            "java.util.Map<String, Object> s = new java.util.HashMap<>();\n    switch (p1) {\n      case \"I32\", \"Int\" -> { s.put(\"k\", \"i32\"); s.put(\"v\", Integer.parseInt(p0)); }\n      case \"F64\", \"F32\" -> { s.put(\"k\", \"f64\"); s.put(\"v\", Double.valueOf(p0)); }\n      case \"String\", \"Str\" -> { s.put(\"k\", \"str\"); s.put(\"v\", p0); }\n      case \"Bool\" -> { s.put(\"k\", \"bool\"); s.put(\"v\", p0.equals(\"t\")); }\n      default -> throw new RuntimeException(\"make_scalar: unknown representation key \" + p1);\n    }\n    return s;"
                .to_owned()
        }
        CanonicalKind::ScalarOf { kind } => format!(
            "java.util.Map<String, Object> s = new java.util.HashMap<>();\n    s.put(\"k\", \"{kind}\");\n    s.put(\"v\", p0);\n    return s;"
        ),
        CanonicalKind::ScalarAs { kind } => {
            let (v0, v1) = sum_ctors(&m.ret_type);
            let project = match kind.as_str() {
                "i32" => "((Number) __m.get(\"v\")).intValue()",
                "f64" => "((Number) __m.get(\"v\")).doubleValue()",
                "str" => "(String) __m.get(\"v\")",
                "bool" => "(Boolean) __m.get(\"v\")",
                _ => "__m.get(\"v\")",
            };
            format!(
                "java.util.Map<?, ?> __m = (java.util.Map<?, ?>) p0;\n    if (\"{kind}\".equals(__m.get(\"k\"))) {{\n      return new {v1}<>({project});\n    }}\n    return new {v0}<>(new Shapes.Unit());"
            )
        }
        CanonicalKind::ScalarIsTrue => {
            "return Boolean.TRUE.equals(((java.util.Map<?, ?>) p0).get(\"v\"));".to_owned()
        }
        CanonicalKind::Custom => format!(
            "throw new RuntimeException(\"kio-test-runner-java: no canonical impl for host fn `{}`\");",
            m.name
        ),
    }
}

fn numeric_to_string_body(kind: &str) -> String {
    match kind {
        "f32" => "return Driver.floatToString((double) p0);".to_owned(),
        "f64" => "return Driver.floatToString(p0);".to_owned(),
        // u64 / i128 / u128 arrive as BigInteger.
        "i128" | "u128" | "u64" => "return p0.toString();".to_owned(),
        _ => "return String.valueOf(p0);".to_owned(),
    }
}

fn int_arith_body(op: &str, kind: &str) -> String {
    // BigInteger widths: compute exactly, wrap to the role's width.
    if matches!(kind, "i128" | "u128" | "u64") {
        let signed = kind.starts_with('i');
        let bits = if kind == "u64" { 64 } else { 128 };
        let bigop = match op {
            "add" => "add(p1)".to_owned(),
            "sub" => "subtract(p1)".to_owned(),
            "mul" => "multiply(p1)".to_owned(),
            "div" => {
                return format!(
                    "return Driver.wrapInt(Driver.truncDiv(p0, p1), {bits}, {signed});"
                );
            }
            "mod" => {
                return format!(
                    "return Driver.wrapInt(Driver.truncMod(p0, p1), {bits}, {signed});"
                );
            }
            _ => "add(p1)".to_owned(),
        };
        return format!("return Driver.wrapInt(p0.{bigop}, {bits}, {signed});");
    }
    let sym = match op {
        "add" => "+",
        "sub" => "-",
        "mul" => "*",
        "div" => "/",
        "mod" => "%",
        _ => "+",
    };
    // Java's fixed-width int / long wrap natively and `/` `%` truncate;
    // the narrower widths re-narrow the widened result, and the
    // unsigned-in-wider-signed widths mask to the role's modulus.
    match kind {
        "i8" => format!("return (byte) (p0 {sym} p1);"),
        "i16" => format!("return (short) (p0 {sym} p1);"),
        "i32" => format!("return p0 {sym} p1;"),
        "i64" => format!("return p0 {sym} p1;"),
        "u8" => format!("return (short) ((p0 {sym} p1) & 0xFF);"),
        "u16" => format!("return (p0 {sym} p1) & 0xFFFF;"),
        "u32" => format!("return (p0 {sym} p1) & 0xFFFFFFFFL;"),
        _ => format!("return p0 {sym} p1;"),
    }
}

fn float_arith_body(op: &str) -> String {
    let sym = match op {
        "add" => "+",
        "sub" => "-",
        "mul" => "*",
        "div" => "/",
        _ => "+",
    };
    format!("return p0 {sym} p1;")
}

fn cmp_body(cmp: &str, kind: &str) -> String {
    if matches!(kind, "i128" | "u128" | "u64") {
        let pred = match cmp {
            "eq" => "== 0",
            "lt" => "< 0",
            "leq" | "le" => "<= 0",
            "gt" => "> 0",
            "geq" | "ge" => ">= 0",
            _ => "== 0",
        };
        return format!("return p0.compareTo(p1) {pred};");
    }
    if cmp == "eq" {
        let primitive_accessor = match kind {
            "i8" => Some("byteValue"),
            "i16" | "u8" => Some("shortValue"),
            "i32" | "u16" => Some("intValue"),
            "i64" | "u32" => Some("longValue"),
            "f32" => Some("floatValue"),
            "f64" => Some("doubleValue"),
            _ => None,
        };
        if let Some(accessor) = primitive_accessor {
            return format!("return p0.{accessor}() == p1.{accessor}();");
        }
    }
    let sym = match cmp {
        "eq" => "==",
        "lt" => "<",
        "leq" | "le" => "<=",
        "gt" => ">",
        "geq" | "ge" => ">=",
        _ => "==",
    };
    format!("return p0 {sym} p1;")
}

/// `loop`: drive the typed step callback until it returns the exit arm.
fn render_loop_body(m: &TraitMethod) -> String {
    let cbret = m
        .where_clause
        .strip_prefix("__cbret=")
        .unwrap_or(&m.ret_type)
        .to_owned();
    format!(
        "class LoopState {{\n      s state = p1;\n      r result;\n      boolean done;\n      void accept({cbret} next) {{\n        next.<Shapes.Unit>KioMatch(\n            value -> {{ state = value; return new Shapes.Unit(); }},\n            value -> {{ result = value; done = true; return new Shapes.Unit(); }});\n      }}\n    }}\n    LoopState state = new LoopState();\n    while (!state.done) {{\n      state.accept(p0.apply(state.state));\n    }}\n    return state.result;"
    )
}

fn render_array_body(op: &ArrayOp, m: &TraitMethod) -> String {
    // The external host owns the concrete carrier. Its generated exact-QTN
    // marker stays public while this witness unwraps only its own carrier.
    let recv = "KioRunnerArray<KioCallType_0> carrier = (KioRunnerArray<KioCallType_0>) p0;\n    java.util.ArrayList<KioCallType_0> a = carrier.value();\n    ";
    match op {
        ArrayOp::MakeEmpty => {
            "return new KioRunnerArray<>(new java.util.ArrayList<>());".to_owned()
        }
        ArrayOp::MakeFilled => "if (p0 < 0) {\n      throw new RuntimeException(\"array_make_filled: negative size \" + p0);\n    }\n    java.util.ArrayList<KioCallType_0> a = new java.util.ArrayList<>();\n    for (int i = 0; i < p0; i++) {\n      a.add(p1);\n    }\n    return new KioRunnerArray<>(a);".to_owned(),
        ArrayOp::Len => format!("{recv}return a.size();"),
        ArrayOp::Get => format!("{recv}if (p1 < 0 || p1 >= a.size()) {{\n      throw new RuntimeException(\"array_get: index \" + p1 + \" out of bounds (len \" + a.size() + \")\");\n    }}\n    return a.get(p1);"),
        ArrayOp::Set => format!("{recv}if (p1 < 0 || p1 >= a.size()) {{\n      throw new RuntimeException(\"array_set: index \" + p1 + \" out of bounds (len \" + a.size() + \")\");\n    }}\n    a.set(p1, p2);"),
        ArrayOp::Push => format!("{recv}a.add(p1);"),
        ArrayOp::PopBack => {
            let (v0, v1) = sum_ctors(&m.ret_type);
            format!("{recv}if (a.isEmpty()) {{\n      return new {v1}<>(new Shapes.Unit());\n    }}\n    return new {v0}<>(a.remove(a.size() - 1));")
        }
        ArrayOp::Swap => format!("{recv}if (p1 < 0 || p1 >= a.size() || p2 < 0 || p2 >= a.size()) {{\n      throw new RuntimeException(\"array_swap: index out of bounds (len \" + a.size() + \")\");\n    }}\n    KioCallType_0 tmp = a.get(p1);\n    a.set(p1, a.get(p2));\n    a.set(p2, tmp);"),
        ArrayOp::Clear => format!("{recv}a.clear();"),
        ArrayOp::Clone => {
            format!("{recv}return new KioRunnerArray<>(new java.util.ArrayList<>(a));")
        }
    }
}

fn javac_command() -> &'static str {
    "javac"
}

fn java_command() -> &'static str {
    "java"
}

fn temp_dir(label: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir = env::temp_dir().join(format!(
        "kio-test-runner-java-{label}-{}-{nanos}",
        process::id()
    ));
    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javac_uses_the_shared_outer_observer_shape() {
        let observer = CompilerObserver::for_test("observe");
        let command = javac_compile_command(&observer);
        assert_eq!(command.get_program(), "observe");
        assert_eq!(command.get_args().collect::<Vec<_>>(), [javac_command()]);
    }

    #[test]
    fn main_call_uses_the_exact_declaring_module() {
        assert_eq!(java_main_call("main"), "    pkg.main.main();\n");
        assert_eq!(
            java_main_call("testapi/main"),
            "    pkg.testapi.KioModule_main.main();\n"
        );
        assert_eq!(java_main_call("prog"), "    pkg.prog.main();\n");
        assert_eq!(java_main_call("api"), "    pkg.api.main();\n");
    }

    #[test]
    fn export_driver_uses_role_framed_facade_selectors() {
        let body = render_export_driver(
            ExportDriver::NewtypeVisibilityFacade,
            "pkg.testapi",
            true,
            &[],
        )
        .expect("driver");
        assert!(body.contains("pkg.testapi.KioModule_types.makeA"));
        assert!(
            body.contains(
                "pkg.testapi.KioModule_types.KioType_ConstructorOnly.makeConstructorOnly"
            )
        );
        assert_eq!(
            java_bare_shell("Product", &["A", "B"]),
            "KioFacade_V1_Product_K2_B1_AB1_B"
        );
        assert_eq!(
            java_positional_shell("Sum", 10),
            "KioFacade_V1_Sum_Positional_K10"
        );
    }

    #[test]
    fn public_word_names_driver_pins_affixes_and_nested_selectors() {
        let body =
            render_export_driver(ExportDriver::PublicWordNames, "pkg", false, &[]).expect("driver");
        for literal in [
            "pkg.wordApi.readWord()",
            "pkg.wordApi._readWord()",
            "pkg.wordApi.readWord_()",
            "pkg.wordApi._readWord_()",
            "pkg.wordApi.readWord__()",
            "KioModule_wordNodes.KioType__uWordBox_u_u.wrapWord(55)",
            "KioModule_wordNodes.KioType__uWordBox_u_u.unwrapWord(boxed)",
            "KioModule_wordNodes.keepWord(66)",
            "wordApi.keepPair(",
            "unwrapWord(pair._WordBox__())",
            "unwrapWord(pair.KioQualified_wordApi_sotherNodes___uWordBox_u_u())",
        ] {
            assert!(body.contains(literal), "{body}");
        }
    }

    #[test]
    fn existential_newtype_driver_uses_exact_generic_sam_instances() {
        let contract = RunnerProtocol::NewtypeVisibilityFacade.contract();
        let body = render_export_driver(
            ExportDriver::NewtypeVisibilityFacade,
            "pkg.testapi",
            true,
            contract.host_types,
        )
        .expect("newtype visibility driver");
        let selected_i32 = "KioSelected_testapi__I32";

        assert!(
            body.contains(&format!(
                "new Shapes.KioForall_H8f1ae0de6f219de2<{selected_i32}, Object>()"
            )),
            "{body}"
        );
        assert!(
            body.contains(
                "@Override public <KioPolyType_1> Object apply(KioPolyType_1 value) { return 89; }"
            ),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                "new Shapes.KioForall_H8b5e594f5b106bfa<{selected_i32}, Object>()"
            )),
            "{body}"
        );
        assert!(
            body.contains("@Override public <KioPolyType_1> Object apply() { return 90; }"),
            "{body}"
        );
        assert!(!body.contains("value -> 89"), "{body}");
        assert!(!body.contains("() -> 90"), "{body}");
    }

    #[test]
    fn rejects_removed_main_entry_option() {
        assert_eq!(run(&["--main-entry=function".to_owned()]), EXIT_USAGE);
    }

    #[test]
    fn loop_signature_keeps_the_exact_function_identity() {
        let binding = HostFnBinding {
            module: "misleading/module",
            leaf: "not_loop",
            body: HostFnBodyKind::Loop,
        };

        let method = java_canonical_method(&binding, &[]);
        assert_eq!(method.name, java_host_member(binding.module, binding.leaf));
    }

    #[test]
    fn higher_kinded_fixture_names_the_exact_nominal_constructor() {
        let contract = RunnerProtocol::HostFunctorDictRoundtrip.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::RoundFunctor))
            .expect("round-functor binding");
        let method = java_method(binding, contract.host_types);
        let expected = "Shapes.KioNewtype_testapi_stypes__Functor<String, Shapes.KioNewtypeMk_testapi_stypes__Box<String>>";

        assert_eq!(method.arg_types, [expected]);
        assert_eq!(method.ret_type, expected);
    }

    #[test]
    fn rank_n_fixture_pins_the_exact_generic_sam_api() {
        let contract = RunnerProtocol::HostRanknRoundtrip.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::ApplyPoly { .. }))
            .expect("apply-poly binding");
        let method = java_method(binding, contract.host_types);

        assert_eq!(
            method.arg_types,
            ["Shapes.KioForall_Site_M_testapi_sarith__H_apply_upoly_Binders_K1_B0_A0<String>"]
        );
        assert_eq!(method.ret_type, "String");
        assert!(!method.arg_types[0].contains("Object"));
    }

    #[test]
    fn returned_forall_fixture_uses_the_exact_generic_sam_wrapper() {
        let contract = RunnerProtocol::ReturnedForallCallByValue.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::ReturnedForallUnit))
            .expect("returned-forall binding");
        let method = java_method(binding, contract.host_types);
        let expected = "Shapes.KioForall_Site_M_testapi_smain__H_produce_Binders_K1_B0_A0";

        assert_eq!(method.ret_type, expected);
        let body = render_java_body_for_binding(
            binding,
            &canonical_kind(binding.body),
            &method,
            contract.host_types,
        );
        assert!(body.contains(&format!("return new {expected}()")), "{body}");
        assert!(
            body.contains("public <KioPolyType_0> KioPolyType_0 apply()"),
            "{body}"
        );
    }

    #[test]
    fn coexist_fixture_implements_both_exact_host_type_adapters() {
        let host = render_coexist_host(
            "HostA",
            "pkg_alpha",
            "PkgAlpha",
            "first: ",
            RunnerProtocol::Coexist.contract(),
        );

        assert!(
            host.contains("implements pkg_alpha.PkgAlphaHost<Integer, String>"),
            "{host}"
        );
        assert!(
            host.contains("KioHostBinding_greeter__I32_fromBody"),
            "{host}"
        );
        assert!(
            host.contains("KioHostBinding_greeter__String_toBody"),
            "{host}"
        );
        assert!(host.contains("void greeter__print(String p0)"), "{host}");
    }

    #[test]
    fn export_drivers_use_generic_curried_functions_and_flatten_products() {
        let nested_contract = RunnerProtocol::NestedCurriedRoundtrip.contract();
        let nested = render_export_driver(
            ExportDriver::NestedCurriedRoundtrip,
            "pkg.testapi",
            true,
            nested_contract.host_types,
        )
        .expect("nested-curried driver");
        assert!(
            nested.contains("Shapes.Fn1<String, Shapes.Fn1<String, String>> join"),
            "{nested}"
        );

        let alias_contract = RunnerProtocol::ModuleAliasScopeCollision.contract();
        let alias = render_export_driver(
            ExportDriver::ModuleAliasScopeCollision,
            "pkg.testapi",
            true,
            alias_contract.host_types,
        )
        .expect("module-alias driver");
        assert!(
            alias.contains("consume(aValue._0(), aValue._1())"),
            "{alias}"
        );
        assert!(
            alias.contains("consume(bValue._0(), bValue._1(), bValue._2())"),
            "{alias}"
        );
    }

    #[test]
    fn string_code_at_boxes_the_integer_codepoint() {
        let protocol = RunnerProtocol::ALL
            .iter()
            .copied()
            .find(|protocol| {
                protocol
                    .contract()
                    .host_fns
                    .iter()
                    .any(|binding| matches!(binding.body, HostFnBodyKind::StringCodeAt { .. }))
            })
            .expect("a protocol with string-code-at");
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| matches!(binding.body, HostFnBodyKind::StringCodeAt { .. }))
            .expect("string-code-at binding");
        let method = java_method(binding, contract.host_types);
        let body = render_java_body_for_binding(
            binding,
            &canonical_kind(binding.body),
            &method,
            contract.host_types,
        );

        assert!(body.contains("<>((int) p0.charAt(p1))"), "{body}");
        assert!(!body.contains("<>(p0.charAt(p1))"), "{body}");
    }

    #[test]
    fn numeric_comparisons_use_primitive_value_semantics() {
        for (kind, accessor) in [
            ("i8", "byteValue"),
            ("i16", "shortValue"),
            ("i32", "intValue"),
            ("i64", "longValue"),
            ("u8", "shortValue"),
            ("u16", "intValue"),
            ("u32", "longValue"),
            ("f32", "floatValue"),
            ("f64", "doubleValue"),
        ] {
            assert_eq!(
                cmp_body("eq", kind),
                format!("return p0.{accessor}() == p1.{accessor}();"),
                "{kind} equality must compare the boxed role's primitive value"
            );
        }

        for kind in ["i128", "u64", "u128"] {
            assert_eq!(
                cmp_body("eq", kind),
                "return p0.compareTo(p1) == 0;",
                "{kind} equality must retain BigInteger value comparison"
            );
        }

        assert_eq!(cmp_body("lt", "i32"), "return p0 < p1;");
        assert_eq!(cmp_body("leq", "f64"), "return p0 <= p1;");
    }
}

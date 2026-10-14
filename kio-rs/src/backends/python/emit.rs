//! Kio' → Python lowering.

use crate::ast::Routed;

const RUNTIME: &str = include_str!("runtime.py");

#[cfg(test)]
mod runtime_tests {
    use super::RUNTIME;

    #[test]
    fn control_blocks_do_not_reenter_an_ast_evaluator() {
        let spine = RUNTIME
            .split_once("    def _run_control(")
            .unwrap()
            .1
            .split_once("    def _enter_guest(")
            .unwrap()
            .0;
        assert!(spine.contains("while True:"));
        assert!(spine.contains("slots[operation[\"output\"]] = slots[operation[\"input\"]]"));
        assert!(spine.contains("block_index = exit[\"block\"]"));
        assert!(
            spine
                .contains("block_index = exit[\"then_block\"] if native else exit[\"else_block\"]")
        );
        assert!(spine.contains("block_index = exit[\"arms\"][variant]"));
        assert!(!spine.contains("self._run_control("));
        assert!(!RUNTIME.contains("self.eval("));
        assert!(!RUNTIME.contains("    def eval("));
    }

    #[test]
    fn product_projection_does_not_collect_argument_slots() {
        let projection = RUNTIME
            .split_once("def _product_slot(")
            .unwrap()
            .1
            .split_once("def _sum_inject(")
            .unwrap()
            .0;
        assert!(!projection.contains("_product_slots("));
    }

    #[test]
    fn guest_binding_does_not_collect_argument_slots() {
        let callable = RUNTIME
            .split_once("class _KioFunction(KioFn):")
            .unwrap()
            .1
            .split_once("class _KioPoly(")
            .unwrap()
            .0;
        let guest = RUNTIME
            .split_once("    def _enter_guest(")
            .unwrap()
            .1
            .split_once("    def _literal(")
            .unwrap()
            .0;
        assert!(!callable.contains("_product_slots("));
        assert!(!guest.contains("_product_slots("));
        assert!(!guest.contains("type(callee)"));
        assert!(!guest.contains("_kio_tail_entry_v1"));
        assert!(!guest.contains("isinstance("));
        assert!(guest.contains("local = dict(callee.env)"));
        assert!(guest.contains(
            "return self._run_control(callee.module, graph, graph[\"start\"], slots, transfer)"
        ));
        assert!(!guest.contains("self.eval("));
        assert!(RUNTIME.contains("self.runtime._enter_guest(self, domain, False, transfer)"));
        assert!(RUNTIME.contains("self.runtime._enter_guest(self, None, True, transfer)"));

        let host = RUNTIME
            .split_once("    def _host_fn_value(")
            .unwrap()
            .1
            .split_once("    def _call_prepared_host(")
            .unwrap()
            .0;
        assert!(host.contains("_product_slots("));
    }

    #[test]
    fn checked_control_exits_separate_tail_and_pending_stages() {
        let evaluator = RUNTIME
            .split_once("    def _run_control(")
            .unwrap()
            .1
            .split_once("    def _enter_guest(")
            .unwrap()
            .0;
        assert!(evaluator.contains("exit[\"stage\"] not in (\"value\", \"type\")"));
        let (tail, pending) = evaluator
            .split_once("                resume = _KioBlockResume")
            .unwrap();
        assert!(tail.contains("if exit[\"resume\"] is None:"));
        assert!(tail.contains("transfer.advance(callee)"));
        assert!(tail.contains("transfer.call(callee, domain)"));
        assert!(!tail.contains("_with_resume("));
        assert!(pending.contains("_kio_advance_with_resume(transfer, callee, resume)"));
        assert!(pending.contains("_kio_call_with_resume(transfer, callee, domain, resume)"));
        assert!(!evaluator.contains("callee.advance()"));
        assert!(!evaluator.contains("callee(domain)"));
    }
}

#[derive(Debug)]
pub struct EmitError {
    pub message: String,
}

impl EmitError {
    fn internal(detail: impl Into<String>) -> Self {
        Self {
            message: detail.into(),
        }
    }
}

/// Lower a typed (post-recovery) Kio package to a single Python module.
/// `namespace` is the effective module stem (`<namespace>.py`), from which
/// the public factory's value brand derives. The default namespace derives
/// from the package name via `default_python_namespace`; the `namespace`
/// build-block key supplies an explicit override.
pub fn lower_package_to_module(
    package: &crate::pass::resolve::Package<Routed>,
    namespace: &str,
) -> Result<String, EmitError> {
    lower_package_to_module_with_signature(package, namespace, None)
}

/// Lower a package with sealed signature history available to shared boundary
/// preparation. Python emits only live declarations; history-only sites and
/// their dependencies have no runtime representation or execution path.
pub fn lower_package_to_module_with_signature(
    package: &crate::pass::resolve::Package<Routed>,
    namespace: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
) -> Result<String, EmitError> {
    let stub = super::stub::render_stub_artifact(package, namespace, sig)
        .map_err(|error| EmitError::internal(error.message))?;
    lower_module_with_stage_bases(
        package,
        namespace,
        sig,
        &stub.stage_bases,
        &stub.public_aliases,
    )
}

/// Render runtime and checker views together. The stub records the exact stage
/// bases while rendering; the runtime publishes that same inventory without
/// parsing generated annotations or repeating stub preparation.
pub fn lower_package(
    package: &crate::pass::resolve::Package<Routed>,
    namespace: &str,
) -> Result<(String, super::stub::PythonStubPackage), EmitError> {
    let stub = super::stub::render_stub_artifact(package, namespace, None)
        .map_err(|error| EmitError::internal(error.message))?;
    let module = lower_module_with_stage_bases(
        package,
        namespace,
        None,
        &stub.stage_bases,
        &stub.public_aliases,
    )?;
    let stub = stub
        .into_package()
        .map_err(|error| EmitError::internal(error.message))?;
    Ok((module, stub))
}

fn lower_module_with_stage_bases(
    package: &crate::pass::resolve::Package<Routed>,
    namespace: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
    stage_bases: &[String],
    public_aliases: &std::collections::BTreeMap<String, String>,
) -> Result<String, EmitError> {
    let ir = crate::backends::serialized_runtime_ir::python_package_ir(
        package,
        sig.map(|(_, replayed)| replayed),
    )
    .map_err(EmitError::internal)?;
    let ir_json = serde_json::to_string(&ir)
        .map_err(|e| EmitError::internal(format!("cannot encode Python IR: {e}")))?;

    let mut out = String::new();
    out.push_str("# Generated by kio - do not edit by hand.\n");
    out.push_str("import json\n\n");
    out.push_str(RUNTIME);
    for name in stage_bases {
        out.push_str(&format!(
            "\n{name} = type({}, (_KioTypeStage,), {{}})\n{name}Source = {name}\n",
            python_string_literal(name),
        ));
    }
    out.push_str("\n\n_KIO_IR = json.loads(");
    out.push_str(&python_chunked_string_literal(&ir_json));
    out.push_str(")\n");
    out.push_str("_KIO_PACKAGE = _CompiledPackage(_KIO_IR)\n\n");
    out.push_str("_KIO_PACKAGE.install_declarations(globals())\n\n");
    for (alias, target) in public_aliases {
        out.push_str(&format!("{alias} = {target}\n"));
    }
    out.push('\n');
    out.push_str(&format!(
        "def create_{}(host=None):\n",
        crate::backends::namespace::value_brand(namespace)
    ));
    out.push_str("    return _KIO_PACKAGE.create(host)\n");
    Ok(out)
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

fn python_chunked_string_literal(s: &str) -> String {
    const CHUNK_CHARS: usize = 1000;
    let mut out = String::from("(\n");
    let mut chunk = String::new();
    for ch in s.chars() {
        chunk.push(ch);
        if chunk.chars().count() >= CHUNK_CHARS {
            out.push_str("    ");
            out.push_str(&python_string_literal(&chunk));
            out.push('\n');
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        out.push_str("    ");
        out.push_str(&python_string_literal(&chunk));
        out.push('\n');
    }
    out.push(')');
    out
}

//! Rendering of the finite control witnesses; expectations live in control.rs.

use crate::ast::UfcsSpelling;
use crate::control::{
    Block, ControlProbe, Descriptor, Payload, Receiver, RecursionMode, RecursionProbe, Sequence,
    Step, TypeProbe, Value,
};

fn quoted(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}
fn value(v: &Value) -> String {
    match v {
        Value::Literal(s) => quoted(s),
        Value::Current => "value".into(),
    }
}
fn receiver(seq: &Sequence) -> String {
    match seq.receiver {
        Receiver::Direct => "bind".into(),
        Receiver::Factory => format!("factory({})", quoted(&seq.initial)),
        Receiver::Projected => format!("Receiver.get(wrapped_factory({}))", quoted(&seq.initial)),
    }
}

fn schedule(seq: &Sequence, depth: usize) -> String {
    let mut out = format!("let value = {};\n", quoted(&seq.initial));
    for (i, step) in seq.steps.iter().enumerate() {
        let event = quoted(&format!("value-{i}:{}", seq.initial));
        out.push_str(&match step {
            Step::Let(v) if i % 2 == 0 => format!("let .local. = {}; let value = local1;\n", value(v)),
            Step::Let(v) => format!("let value = {};\n", value(v)),
            Step::Bind(Payload::String(v)) if i % 2 == 0 => format!("let .local. <- effect({event}, ((), {})); let value = local2;\n", value(v)),
            Step::Bind(Payload::String(v)) => format!("let value <- effect({event}, {});\n", value(v)),
            Step::Bind(Payload::Unit) => format!("let unit <- effect({event}, ());\n"),
            Step::Action => format!("action({});\n", quoted(&format!("action-{i}:{}", seq.initial))),
            Step::Nested(inner) => format!("let nested{depth} = do! {} {{ {} }};\nlet observed{depth} = io.print(Box.get(nested{depth}));\n", receiver(inner), schedule(inner, depth + 1)),
        });
    }
    out.push_str(&format!("pure({})", value(&seq.result)));
    out
}

fn descriptor(d: Descriptor) -> &'static str {
    match d {
        Descriptor::Product => "product",
        Descriptor::Thunk => "thunk",
        Descriptor::Sequence => "sequence",
    }
}
fn slot_type(b: &Block) -> &'static str {
    match b {
        Block::Product(v) => match v.len() {
            0 => ".",
            1 => "String",
            2 => "String & String",
            _ => unreachable!("bounded product"),
        },
        Block::Thunk { .. } => ". -> String",
        Block::Sequence(_) => "Bind(Box) -> Box(String)",
    }
}
fn block(b: &Block) -> String {
    match b {
        Block::Product(entries) => entries
            .iter()
            .map(|v| format!("entry({})", quoted(v)))
            .collect::<Vec<_>>()
            .join("; "),
        Block::Thunk { initial, result } => format!(
            "let value = {}; io.print(\"thunk\\n\"); {}",
            quoted(initial),
            value(result)
        ),
        Block::Sequence(seq) => schedule(seq, 0),
    }
}
fn observe(b: &Block, name: &str) -> String {
    match b {
        Block::Product(entries) => match entries.len() {
            0 => format!("observe_unit({name});\n"),
            1 => format!("io.print({name});\n"),
            2 => format!("let .(a, b) = {name}; io.print(a); io.print(b);\n"),
            _ => unreachable!("bounded product"),
        },
        Block::Thunk { .. } => format!("io.print({name}());\n"),
        Block::Sequence(seq) => format!("io.print(Box.get({name}({})));\n", receiver(seq)),
    }
}

fn declaration(name: &str, first: Descriptor, second: Descriptor, label: &str) -> String {
    format!(
        "pub elab {name} : [Left][Right] ((Left -> Right -> .) & Left & Right) -> . {{ trailing {}; trailing {} {label}; impl(fills) apply }}\n",
        descriptor(first),
        descriptor(second)
    )
}

pub fn files(probe: &ControlProbe) -> Vec<(String, String)> {
    if let ControlProbe::Surface {
        control,
        types,
        recursion,
    } = probe
    {
        let mut files = files(control);
        let runtime = &mut files
            .iter_mut()
            .find(|(path, _)| path == "workdir/kio_gen_control.kio")
            .unwrap()
            .1;
        if matches!(control.as_ref(), ControlProbe::Sequence(_)) {
            *runtime = runtime.replace("pub fn run()", "fn control_value() -> String");
            *runtime = runtime.replace("io.print(Box.get(result)) }", "Box.get(result) }");
            runtime.push_str("pub fn run() { let value = control_value(); io.print(value);");
        } else {
            *runtime = runtime.replace("pub fn run()", "fn control_run()");
            runtime.push_str(&format!(
                "pub fn run() {{ control_run(); let value = {};",
                quoted(types.initial_value())
            ));
        }
        runtime.push_str(" let selected = type_probe.transform(value); io.print(selected);");
        if recursion.is_some() {
            runtime.push_str(" io.print(recursion_probe.transform(selected));");
        }
        runtime.push_str(" () }\n");
        *runtime = runtime.replace(
            "import testapi(String);",
            "import testapi(String);\nimport kio_gen_types as type_probe;",
        );
        files.push(("workdir/kio_gen_types.kio".into(), type_probe(types)));
        if let Some(probe) = recursion {
            let runtime = &mut files
                .iter_mut()
                .find(|(path, _)| path == "workdir/kio_gen_control.kio")
                .unwrap()
                .1;
            *runtime = runtime.replace(
                "import testapi(String);",
                "import testapi(String);\nimport kio_gen_recursion as recursion_probe;",
            );
            files.push((
                "workdir/kio_gen_recursion.kio".into(),
                recursion_probe(probe),
            ));
            files.push(("workdir/testapi/iter.kio".into(), "module testapi/iter;\nhost fn loop[S][R](step: S -> (S | R), state: S) -> R;\nhost fn loop_reentry[S][R](step: ((S -> R) & S) -> (S | R), state: S) -> R;\n".into()));
        }
        return files;
    }
    let mut declarations = String::from(
        r#"module kio_gen_control_blocks;
import __comptime__;

pure fn apply(ct: __Comptime__, fills: __Fill_ctx__, left_type: __Type__, right_type: __Type__, arguments: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {
  let result = __term_let__(ct, __term_type__(ct, arguments), arguments, .(packet: __Checked_term__) -> __Checked_term__ {
    let consumer = __intrinsic_fst__(ct, __term_type__(ct, packet), packet);
    let blocks = __intrinsic_snd__(ct, __term_type__(ct, packet), packet);
    let first = __intrinsic_fst__(ct, __term_type__(ct, blocks), blocks);
    let second = __intrinsic_snd__(ct, __term_type__(ct, blocks), blocks);
    let partial = __term_call__(ct, __term_type__(ct, consumer), consumer, first);
    __term_call__(ct, __term_type__(ct, partial), partial, second)
  });
  (result, fills)
}
"#,
    );
    let mut runtime = String::from(
        r#"module kio_gen_control;
import testapi(String);
import testapi/io as io;
import sequence(Bind, do);
import kio_gen_control_blocks(probe, nested_probe);

newtype Box[A] : A { constructor mk; projector get };
newtype Receiver : Bind(Box) { constructor mk; projector get };
fn pure[A](v: A) -> Box(A) { Box.mk(v) }
fn bind[A][B](v: Box(A), k: A -> Box(B)) -> Box(B) { io.print("bind\n"); k(Box.get(v)) }
fn factory(label: String) -> Bind(Box) { io.print("receiver:"); io.print(label); bind }
fn wrapped_factory(label: String) -> Receiver { io.print("receiver:"); io.print(label); Receiver.mk(bind) }
fn effect[A](label: String, v: A) -> Box(A) { io.print(label); pure(v) }
fn action(label: String) -> Box(.) { io.print(label); pure(()) }
fn entry(v: String) -> String { io.print("entry:"); io.print(v); v }
fn observe_unit(v: .) { io.print("unit\n") }
fn nested_consumer(first: . -> String)(second: . -> String) { io.print("consumer\n"); io.print(first()); io.print(second()) }
"#,
    );
    declarations.push_str(&declaration(
        "nested_probe",
        Descriptor::Thunk,
        Descriptor::Thunk,
        "after_nested",
    ));
    match probe {
        ControlProbe::Surface { .. } => unreachable!("surface probe rendered above"),
        ControlProbe::Sequence(seq) => {
            // Keep both imported names valid without adding an unused descriptor arm.
            declarations.push_str(&declaration(
                "probe",
                Descriptor::Thunk,
                Descriptor::Thunk,
                "after_unused",
            ));
            runtime.push_str(&format!(
                "pub fn run() {{ let result = do! {} {{ {} }}; io.print(Box.get(result)) }}\n",
                receiver(seq),
                schedule(seq, 0)
            ));
        }
        ControlProbe::Blocks {
            first,
            second,
            label,
            reverse,
            nested,
        } => {
            declarations.push_str(&declaration(
                "probe",
                first.descriptor(),
                second.descriptor(),
                label,
            ));
            runtime.push_str(&format!(
                "fn consumer(first: {})(second: {}) {{\nio.print(\"consumer\\n\");\n",
                slot_type(first),
                slot_type(second)
            ));
            for (b, name) in if *reverse {
                [(second, "second"), (first, "first")]
            } else {
                [(first, "first"), (second, "second")]
            } {
                runtime.push_str(&observe(b, name));
            }
            if *nested {
                runtime.push_str("nested_probe! nested_consumer { io.print(\"thunk\\n\"); \"nested-first\\n\" } after_nested { io.print(\"thunk\\n\"); \"nested-second\\n\" };\n");
            }
            runtime.push_str("()\n}\n");
            runtime.push_str(&format!(
                "pub fn run() {{ probe! consumer {{ {} }} {label} {{ {} }} }}\n",
                block(first),
                block(second)
            ));
        }
    }
    vec![
        ("workdir/kio_gen_control.kio".into(), runtime),
        ("workdir/kio_gen_control_blocks.kio".into(), declarations),
        (
            "workdir/sequence.kio".into(),
            include_str!("../../../../test-data/poc/elab/workdir/sequence.kio").into(),
        ),
        (
            "workdir/testapi/io.kio".into(),
            "module testapi/io;\nimport testapi(String);\nhost fn print(s: String) -> .;\n".into(),
        ),
    ]
}

fn type_probe(probe: &TypeProbe) -> String {
    let mut out = String::from(
        "module kio_gen_types;\nimport __intrinsics__;\nimport testapi(String);\nimport testapi/io as io;\n\n",
    );
    match probe {
        TypeProbe::Staged {
            spelling,
            explicit,
            first: _,
            last,
        } => {
            out.push_str("fn staged(prefix: String)[A](value: A) -> String { prefix }\n");
            let first = "input";
            let last = quoted(last);
            let call = match spelling {
                UfcsSpelling::ReceiverFirst => format!(
                    "{first}.>staged({}{last})",
                    if *explicit { "String, " } else { "" }
                ),
                UfcsSpelling::ReceiverLast => format!(
                    "{last}.>>staged({first}{})",
                    if *explicit { ", String" } else { "" }
                ),
                UfcsSpelling::CalleeLast => format!(
                    "staged({first}{}) .<{last}",
                    if *explicit { ", String" } else { "" }
                ),
                UfcsSpelling::CalleeFirst => format!(
                    "staged({}{last}).<<{first}",
                    if *explicit { "String, " } else { "" }
                ),
            };
            out.push_str(&format!(
                "pub fn transform(input: String) -> String {{ let selected = {call}; selected }}\n"
            ));
        }
        TypeProbe::HigherRank { .. } => {
            out.push_str("fn apply(transform: [A] A -> A, value: String) -> String { transform(String, value) }\n");
            out.push_str("pub fn transform(input: String) -> String { let identity = .[A](value: A) -> A { value }; apply(identity, input) }\n");
        }
        TypeProbe::HigherKinded { wrapped, .. } => {
            out.push_str("newtype Box[A] : A { constructor mk; projector get };\nnewtype Wrapped[A] : . & A { constructor mk; projector get };\nfn unbox[A](value: Box(A)) -> A { Box.get(value) }\nfn unwrap[A](value: Wrapped(A)) -> A { __snd__(., A, Wrapped.get(value)) }\nfn apply[*F][A](value: F(A), project: [B] F(B) -> B) -> A { project(A, value) }\n");
            let call = if *wrapped {
                "apply(Wrapped, String, Wrapped.mk(((), input)), unwrap)"
            } else {
                "apply(Box, String, Box.mk(input), unbox)"
            };
            out.push_str(&format!(
                "pub fn transform(input: String) -> String {{ {call} }}\n"
            ));
        }
        TypeProbe::Existential {
            payload,
            explicit_open,
            first: _,
            last,
        } => {
            out.push_str("newtype Box[A] : A { constructor mk; projector get };\nnewtype Packed <Hidden> : Hidden & (Hidden -> String) { constructor mk; projector get };\n");
            let packed = match payload {
                0 => "Packed.mk((input, .(seed: String) -> String { seed }))".into(),
                1 => format!(
                    "Packed.mk((({}, {}), .(seed: String & String) -> String {{ __snd__(String, String, seed) }}))",
                    quoted(last),
                    "input"
                ),
                2 => "Packed.mk((Box.mk(input), .(seed: Box(String)) -> String { Box.get(seed) }))"
                    .into(),
                _ => unreachable!("bounded existential payload"),
            };
            out.push_str(&format!(
                "fn make(input: String) -> Packed {{ {packed} }}\n"
            ));
            if *explicit_open {
                out.push_str("fn open(packed: Packed) -> String { Packed.get(packed)(String, .[Hidden](packet: Hidden & (Hidden -> String)) -> String { let .(seed, observe) = packet; observe(seed) }) }\n");
            } else {
                out.push_str("fn open(packed: Packed) -> String { let .(<Hidden> packet) = Packed.get(packed); let .(seed, observe) = packet; observe(seed) }\n");
            }
            out.push_str("pub fn transform(input: String) -> String { let packed = make(input); let selected = open(packed); selected }\n");
        }
    }
    out
}

fn recursion_probe(probe: &RecursionProbe) -> String {
    let mut out = String::from(
        "module kio_gen_recursion;\nimport __intrinsics__;\nimport match(match);\nimport testapi(String);\nimport testapi/io as io;\nimport testapi/iter as iter;\n\nrec newtype Counter : . | Counter { constructor mk; projector get };\nfn identity(value: String) -> String { value }\n",
    );
    let poly = matches!(
        probe.mode,
        RecursionMode::Poly | RecursionMode::PolyPending | RecursionMode::PolyEscape
    );
    let escape = matches!(
        probe.mode,
        RecursionMode::Escape | RecursionMode::PolyEscape
    );
    out.push_str(if escape {
        "rec(iter.loop_reentry) "
    } else {
        "rec(iter.loop) "
    });
    if probe.mutual {
        out.push_str("{\n");
    }
    for name in if probe.mutual {
        vec!["walk", "bounce"]
    } else {
        vec!["walk"]
    } {
        let target = if probe.mutual && name == "walk" {
            "bounce"
        } else {
            "walk"
        };
        out.push_str(&format!(
            "fn {name}{}(state: Counter, value: {}{}) -> {} {{\nlet current = Counter.get(state);\nmatch! current {{ (\n",
            if poly { "[A]" } else { "" },
            if poly { "A" } else { "String" },
            if poly { ", observe: A -> String" } else { "" },
            if escape { ". -> String" } else { "String" }
        ));
        let value = if poly { "observe(value)" } else { "value" };
        let event = |label: &str| {
            quoted(&format!(
                "{}{label}\n",
                if probe.mutual {
                    format!("{name}:")
                } else {
                    String::new()
                }
            ))
        };
        let base = format!("io.print({}); {value}", event("base"));
        let call = if poly {
            format!("{target}(String, rest, observe(value), identity)")
        } else {
            format!("{target}(rest, value)")
        };
        let step = match probe.mode {
            RecursionMode::Tail => format!("io.print({}); recur {call}", event("step")),
            RecursionMode::Pending => format!(
                "io.print({}); let result = recur(cont) {call}; io.print({}); result",
                event("before"),
                event("after")
            ),
            RecursionMode::Poly => format!("io.print({}); recur(poly) {call}", event("step")),
            RecursionMode::PolyPending => format!(
                "io.print({}); let result = recur(poly, cont) {call}; io.print({}); result",
                event("before"),
                event("after")
            ),
            RecursionMode::Escape => {
                format!(
                    ".() -> String {{ io.print({}); recur(escape) {call}() }}",
                    event("escape")
                )
            }
            RecursionMode::PolyEscape => {
                format!(
                    ".() -> String {{ io.print({}); recur(poly, escape) {call}() }}",
                    event("escape")
                )
            }
        };
        out.push_str(&format!(".(rest: Counter) {{ {step} }},\n"));
        out.push_str(&format!(
            ".(_unit: .) {{ {} }}\n) }}\n}}\n",
            if escape {
                format!(".() -> String {{ {base} }}")
            } else {
                base
            }
        ));
        if probe.mutual && name == "walk" {
            out.push_str(";\n");
        }
    }
    if probe.mutual {
        out.push_str("}\n");
    }
    let mut counter = "Counter.mk(__left__(., Counter, ()))".to_string();
    for _ in 0..probe.depth {
        counter = format!("Counter.mk(__right__(., Counter, {counter}))");
    }
    let selected = "input";
    let call = if poly {
        format!(
            "walk(String & ., {counter}, ({selected}, ()), .(packet: String & .) -> String {{ __fst__(String, ., packet) }})"
        )
    } else {
        format!("walk({counter}, {selected})")
    };
    out.push_str(&format!(
        "pub fn transform(input: String) -> String {{ {} }}\n",
        if escape {
            format!("let saved = {call}; io.print(\"saved\\n\"); saved()")
        } else {
            call
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_and_ordinary_bindings_share_the_control_model() {
        let sequence = Sequence {
            receiver: Receiver::Direct,
            initial: "initial\n".into(),
            steps: vec![
                Step::Let(Value::Current),
                Step::Let(Value::Current),
                Step::Bind(Payload::String(Value::Current)),
                Step::Bind(Payload::String(Value::Current)),
            ],
            result: Value::Current,
        };
        let source = schedule(&sequence, 0);
        for form in [
            "let .local. = value; let value = local1;",
            "let value = value;",
            "let .local. <- effect(\"value-2:initial\\n\", ((), value)); let value = local2;",
            "let value <- effect(\"value-3:initial\\n\", value);",
        ] {
            assert!(source.contains(form), "{source}");
        }
        assert_eq!(
            crate::control::expected_stdout(&ControlProbe::Sequence(sequence)),
            "value-2:initial\nbind\nvalue-3:initial\nbind\ninitial\n"
        );
    }
}

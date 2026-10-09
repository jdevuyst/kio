//! Rendering of the finite control witnesses; expectations live in control.rs.

use crate::control::{Block, ControlProbe, Descriptor, Payload, Receiver, Sequence, Step, Value};

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
            Step::Let(v) => format!("let value = {};\n", value(v)),
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

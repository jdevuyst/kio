//! Finite runtime witnesses, independently modelled before source rendering.

use rand::Rng;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Receiver {
    Direct,
    Factory,
    Projected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Literal(String),
    Current,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Payload {
    String(Value),
    Unit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    Let(Value),
    Bind(Payload),
    Action,
    Nested(Box<Sequence>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sequence {
    pub receiver: Receiver,
    pub initial: String,
    pub steps: Vec<Step>,
    pub result: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Block {
    Product(Vec<String>),
    Thunk { initial: String, result: Value },
    Sequence(Sequence),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Descriptor {
    Product,
    Thunk,
    Sequence,
}

impl Block {
    pub fn descriptor(&self) -> Descriptor {
        match self {
            Self::Product(_) => Descriptor::Product,
            Self::Thunk { .. } => Descriptor::Thunk,
            Self::Sequence(_) => Descriptor::Sequence,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlProbe {
    Sequence(Sequence),
    Blocks {
        first: Block,
        second: Block,
        label: String,
        reverse: bool,
        nested: bool,
    },
}

fn value<R: Rng>(rng: &mut R, label: &str) -> Value {
    if rng.gen_bool(0.5) {
        Value::Current
    } else {
        Value::Literal(format!("{label}\n"))
    }
}

fn sequence<R: Rng>(rng: &mut R, nested: bool, tag: &str) -> Sequence {
    let receiver = [Receiver::Direct, Receiver::Factory, Receiver::Projected][rng.gen_range(0..3)];
    let mut steps = Vec::new();
    for i in 0..rng.gen_range(0..=3) {
        if rng.gen_bool(0.5) {
            steps.push(Step::Let(value(rng, &format!("{tag}-let-{i}"))));
        }
        steps.push(match rng.gen_range(0..3) {
            0 => Step::Bind(Payload::String(value(rng, &format!("{tag}-string-{i}")))),
            1 => Step::Bind(Payload::Unit),
            _ => Step::Action,
        });
    }
    if nested && rng.gen_bool(0.25) {
        steps.push(Step::Nested(Box::new(sequence(rng, false, "inner"))));
    }
    Sequence {
        receiver,
        initial: format!("{tag}-initial\n"),
        steps,
        result: value(rng, &format!("{tag}-final")),
    }
}

fn block<R: Rng>(rng: &mut R, tag: &str) -> Block {
    match rng.gen_range(0..3) {
        0 => Block::Product(
            (0..rng.gen_range(0..=2))
                .map(|i| format!("{tag}-product-{i}\n"))
                .collect(),
        ),
        1 => Block::Thunk {
            initial: format!("{tag}-thunk-local\n"),
            result: value(rng, &format!("{tag}-thunk-final")),
        },
        _ => Block::Sequence(sequence(rng, true, tag)),
    }
}

pub fn sample<R: Rng>(rng: &mut R) -> ControlProbe {
    if rng.gen_bool(0.5) {
        ControlProbe::Sequence(sequence(rng, true, "standalone"))
    } else {
        ControlProbe::Blocks {
            first: block(rng, "first"),
            second: block(rng, "second"),
            label: format!("after_slot{}", rng.gen_range(0..32)),
            reverse: rng.gen_bool(0.5),
            nested: rng.gen_bool(0.25),
        }
    }
}

fn lookup<'a>(value: &'a Value, current: &'a str) -> &'a str {
    match value {
        Value::Literal(s) => s,
        Value::Current => current,
    }
}

fn sequence_value(sequence: &Sequence, out: &mut String) -> String {
    if sequence.receiver != Receiver::Direct {
        out.push_str("receiver:");
        out.push_str(&sequence.initial);
    }
    let mut current = sequence.initial.clone();
    for (i, step) in sequence.steps.iter().enumerate() {
        match step {
            Step::Let(value) => current = lookup(value, &current).to_string(),
            Step::Bind(payload) => {
                out.push_str(&format!("value-{i}:{}bind\n", sequence.initial));
                if let Payload::String(value) = payload {
                    current = lookup(value, &current).to_string();
                }
            }
            Step::Action => out.push_str(&format!("action-{i}:{}bind\n", sequence.initial)),
            Step::Nested(inner) => {
                let selected = sequence_value(inner, out);
                out.push_str(&selected);
            }
        }
    }
    lookup(&sequence.result, &current).to_string()
}

fn eager(block: &Block, out: &mut String) {
    if let Block::Product(entries) = block {
        for entry in entries {
            out.push_str("entry:");
            out.push_str(entry);
        }
    }
}

fn observe(block: &Block, out: &mut String) {
    match block {
        Block::Product(entries) => {
            if entries.is_empty() {
                out.push_str("unit\n");
            }
            for entry in entries {
                out.push_str(entry);
            }
        }
        Block::Thunk { initial, result } => {
            out.push_str("thunk\n");
            out.push_str(lookup(result, initial));
        }
        Block::Sequence(sequence) => {
            let result = sequence_value(sequence, out);
            out.push_str(&result);
        }
    }
}

/// This follows sampled schedules and lexical values, not rendered syntax or
/// elaborator lowering. The consumer explicitly invokes delayed blocks.
pub fn expected_stdout(probe: &ControlProbe) -> String {
    let mut out = String::new();
    match probe {
        ControlProbe::Sequence(sequence) => {
            let result = sequence_value(sequence, &mut out);
            out.push_str(&result);
        }
        ControlProbe::Blocks {
            first,
            second,
            reverse,
            nested,
            ..
        } => {
            eager(first, &mut out);
            eager(second, &mut out);
            out.push_str("consumer\n");
            for block in if *reverse {
                [second, first]
            } else {
                [first, second]
            } {
                observe(block, &mut out);
            }
            if *nested {
                out.push_str("consumer\nthunk\nnested-first\nthunk\nnested-second\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_sequence_oracles_cover_scope_and_payload_transitions() {
        let inner = Sequence {
            receiver: Receiver::Direct,
            initial: "inner\n".into(),
            steps: vec![Step::Let(Value::Literal("shadow\n".into()))],
            result: Value::Current,
        };
        let seq = Sequence {
            receiver: Receiver::Factory,
            initial: "outer\n".into(),
            steps: vec![
                Step::Bind(Payload::String(Value::Current)),
                Step::Bind(Payload::Unit),
                Step::Nested(Box::new(inner)),
                Step::Bind(Payload::String(Value::Current)),
            ],
            result: Value::Current,
        };
        assert_eq!(
            expected_stdout(&ControlProbe::Sequence(seq)),
            "receiver:outer\nvalue-0:outer\nbind\nvalue-1:outer\nbind\nshadow\nvalue-3:outer\nbind\nouter\n"
        );
        let zero = ControlProbe::Sequence(Sequence {
            receiver: Receiver::Projected,
            initial: "zero\n".into(),
            steps: vec![],
            result: Value::Current,
        });
        assert_eq!(expected_stdout(&zero), "receiver:zero\nzero\n");
        for wrong in ["zero\n", "receiver:zero\nreceiver:zero\nzero\n"] {
            assert_ne!(expected_stdout(&zero), wrong);
        }
    }

    #[test]
    fn products_are_eager_but_thunks_and_sequences_are_delayed() {
        let probe = ControlProbe::Blocks {
            first: Block::Thunk {
                initial: "local\n".into(),
                result: Value::Current,
            },
            second: Block::Product(vec!["one\n".into(), "two\n".into()]),
            label: "after_0".into(),
            reverse: false,
            nested: true,
        };
        assert_eq!(
            expected_stdout(&probe),
            "entry:one\nentry:two\nconsumer\nthunk\nlocal\none\ntwo\nconsumer\nthunk\nnested-first\nthunk\nnested-second\n"
        );
        let sequence = Block::Sequence(Sequence {
            receiver: Receiver::Factory,
            initial: "seq\n".into(),
            steps: vec![Step::Action],
            result: Value::Current,
        });
        let probe = ControlProbe::Blocks {
            first: Block::Product(vec!["product\n".into()]),
            second: sequence,
            label: "after_7".into(),
            reverse: true,
            nested: false,
        };
        assert_eq!(
            expected_stdout(&probe),
            "entry:product\nconsumer\nreceiver:seq\naction-0:seq\nbind\nseq\nproduct\n"
        );
        assert_ne!(
            expected_stdout(&probe),
            "entry:product\nconsumer\nbind\naction-0:seq\nreceiver:seq\nseq\nproduct\n"
        );
    }
}

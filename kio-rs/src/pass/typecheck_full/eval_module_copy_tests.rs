use super::*;
use std::cell::Cell;
use std::collections::BTreeSet;

thread_local! {
    static CLONED_ITEMS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

pub(super) fn record_item_clone(item: &Item<Lowered>) {
    CLONED_ITEMS.with(|count| {
        let (items, bodies) = count.get();
        count.set((
            items + 1,
            bodies + usize::from(matches!(item, Item::FnDef(_))),
        ));
    });
}

fn clone_then_filter(
    module: &Module,
    selected: &BTreeSet<EvalFnIdentity>,
    module_path: &str,
    item_count: usize,
) -> Module {
    module.items.iter().for_each(record_item_clone);
    let mut module = module.clone();
    module.items.truncate(item_count);
    module.items.retain(|item| match item {
        Item::FnDef(def) => selected.contains(&EvalFnIdentity {
            module_path: module_path.to_owned(),
            name: def.name.clone(),
        }),
        _ => true,
    });
    module
}

fn fixture(discarded_functions: usize, body_statements: usize) -> Module {
    let mut source = String::from(
        "/// Provider documentation\nmodule provider;\n\
         import __comptime__;\n\
         /// Retained type\ntype Value = .;\n\
         pure fn selected(_ct: __Comptime__, _ty: __Type__, value: __Checked_term__)\n\
           -> __Checked_term__ {\n\
           let keep = .(x: __Checked_term__) -> __Checked_term__ { x }; keep(value)\n\
         }\n\
         elab identity : [A] A -> A { impl selected; };\n",
    );
    for index in 0..discarded_functions {
        source.push_str(&format!("pure fn unused{index}(value: .) -> . {{\n"));
        for _ in 0..body_statements {
            source.push_str("let unused = value;\n");
        }
        source.push_str("value }\n");
    }
    source
        .push_str("/// Trailing type\nnewtype Tail : . { constructor tail; projector untail; };\n");
    let module = crate::pass::parser::parse(&source).expect("fixture parses");
    let module = crate::pass::desugar::desugar_module(module).expect("fixture desugars");
    let (mut modules, _) = crate::pass::label_elab::elaborate_package(
        vec![(std::path::PathBuf::from("provider.kio"), module)],
        None,
    )
    .expect("fixture lowers");
    modules.pop().unwrap().1
}

fn selected() -> BTreeSet<EvalFnIdentity> {
    BTreeSet::from([EvalFnIdentity {
        module_path: "provider".to_owned(),
        name: "selected".to_owned(),
    }])
}

#[test]
fn eval_module_copy_preserves_exact_module_and_clones_only_retained_items() {
    let module = fixture(3, 4);
    for path in ["provider", "other"] {
        for item_count in 0..=module.items.len() + 1 {
            let expected = clone_then_filter(&module, &selected(), path, item_count);
            CLONED_ITEMS.with(|count| count.set((0, 0)));
            let actual = filtered_eval_module(&module, &selected(), path, item_count);
            assert_eq!(actual, expected, "path={path}, cutoff={item_count}");
            assert_eq!(
                CLONED_ITEMS.with(Cell::get),
                (
                    actual.items.len(),
                    actual
                        .items
                        .iter()
                        .filter(|item| matches!(item, Item::FnDef(_)))
                        .count(),
                ),
                "discarded items must never be cloned"
            );
        }
    }
}

#[test]
#[ignore = "bounded manual time/RSS comparison of module-copy strategies"]
fn eval_module_copy_cost() {
    let strategy = std::env::var("KIO_DEBUG_EVAL_COPY_STRATEGY").unwrap();
    let size = std::env::var("KIO_DEBUG_EVAL_COPY_SIZE").unwrap();
    let (functions, statements, repetitions) = match size.as_str() {
        "ordinary" => (3, 4, 2000),
        "body" => (24, 128, 200),
        _ => panic!("expected ordinary or body"),
    };
    let module = fixture(functions, statements);
    let selected = selected();
    let copy = match strategy.as_str() {
        "baseline" => clone_then_filter,
        "candidate" => filtered_eval_module,
        _ => panic!("expected baseline or candidate"),
    };
    CLONED_ITEMS.with(|count| count.set((0, 0)));
    let start = Instant::now();
    for _ in 0..repetitions {
        std::hint::black_box(copy(&module, &selected, "provider", module.items.len()));
    }
    let elapsed = start.elapsed();
    let counts = CLONED_ITEMS.with(Cell::get);
    eprintln!(
        "strategy={strategy} size={size} repetitions={repetitions} elapsed_us={} cloned_items={} cloned_bodies={}",
        elapsed.as_micros(),
        counts.0,
        counts.1,
    );
}

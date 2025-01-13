use std::collections::BTreeMap;

use criterion::{criterion_group, criterion_main};
use tellur_core::node::TellurNode;
use tellur_core::tellur_std_node::logical::not::NotNode;
use tellur_core::tree::{NodeId, TellurNodeTree, TreeInput};
use tellur_core::types::{TellurRefType, TellurType, TellurTypedValueContainer};

fn not_tree() -> TellurNodeTree {
    TellurNodeTree {
        name: "not_wrapped".to_string(),
        parameters: BTreeMap::from([(
            "value".to_string(),
            (TellurRefType::Immutable, TellurType::Bool),
        )]),
        returns: BTreeMap::from([("result".to_string(), TellurType::Bool)]),
        nodes: BTreeMap::from([(
            NodeId(0),
            (
                BTreeMap::from([(
                    "value".to_string(),
                    TreeInput::Parameter {
                        name: "value".to_string(),
                    },
                )]),
                Box::new(NotNode {}) as Box<dyn TellurNode>,
            ),
        )]),
        outputs: BTreeMap::from([(
            "result".to_string(),
            TreeInput::NodeOutput {
                id: NodeId(0),
                output_name: "result".to_string(),
            },
        )]),
    }
}

fn plan(c: &mut criterion::Criterion) {
    c.bench_function("Planning Not-Wrapped Tree", |b| {
        b.iter(|| {
            let tree = not_tree();
            let _ = tree.planned();
        })
    });
}

fn run(c: &mut criterion::Criterion) {
    let planned = not_tree().planned();
    let vec = vec![TellurTypedValueContainer::new(
        tellur_core::types::TellurTypedValue::Bool(true).into(),
    )];
    c.bench_function("Running Not-Wrapped Tree", |b| {
        b.iter(|| {
            let _ = planned.evaluate(vec.clone());
        })
    });
}

criterion_group!(benches, plan, run);
criterion_main!(benches);

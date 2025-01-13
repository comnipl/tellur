mod or_with_andnot;
mod wrapped_not;

use criterion::{criterion_group, criterion_main};
use tellur_core::node::TellurNode;
use tellur_core::types::TellurTypedValueContainer;

use self::or_with_andnot::or_with_andnot_tree;
use self::wrapped_not::wrapped_not_tree;

fn plan_wrapped_not(c: &mut criterion::Criterion) {
    c.bench_function("Planning Not-Wrapped Tree", |b| {
        b.iter(|| {
            let tree = wrapped_not_tree();
            let _ = tree.planned();
        })
    });
}

fn run_wrapped_not(c: &mut criterion::Criterion) {
    let planned = wrapped_not_tree().planned();
    let vec = vec![TellurTypedValueContainer::new(
        tellur_core::types::TellurTypedValue::Bool(true).into(),
    )];
    c.bench_function("Running Not-Wrapped Tree", |b| {
        b.iter(|| {
            let _ = planned.evaluate(vec.clone());
        })
    });
}

fn plan_or_with_andnot(c: &mut criterion::Criterion) {
    c.bench_function("Planning Or-With-Andnot Tree", |b| {
        b.iter(|| {
            let tree = or_with_andnot_tree();
            let _ = tree.planned();
        })
    });
}

fn run_or_with_andnot(c: &mut criterion::Criterion) {
    let planned = or_with_andnot_tree().planned();
    let vec = vec![
        TellurTypedValueContainer::new(tellur_core::types::TellurTypedValue::Bool(true).into()),
        TellurTypedValueContainer::new(tellur_core::types::TellurTypedValue::Bool(false).into()),
    ];
    c.bench_function("Running Or-With-Andnot Tree", |b| {
        b.iter(|| {
            let _ = planned.evaluate(vec.clone());
        })
    });
}

criterion_group!(
    benches,
    plan_wrapped_not,
    run_wrapped_not,
    plan_or_with_andnot,
    run_or_with_andnot
);
criterion_main!(benches);

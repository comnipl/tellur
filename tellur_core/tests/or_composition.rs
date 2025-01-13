use std::collections::BTreeMap;

use tellur_core::composition::{ComponentId, Edge, Placement, TellurComposition};
use tellur_core::tellur_std_node::logical::and::AndNode;
use tellur_core::tellur_std_node::logical::not::NotNode;
use tellur_core::types::{TellurRefType, TellurType, TellurTypedValue};

use assert_matches::assert_matches;

#[test]
fn or_composition_should_work() {
    let mut c = TellurComposition::new("or");

    c.mut_parameters().insert(
        "left".to_string(),
        (TellurRefType::Immutable, TellurType::Bool),
    );

    c.mut_parameters().insert(
        "right".to_string(),
        (TellurRefType::Immutable, TellurType::Bool),
    );

    c.mut_returns()
        .insert("result".to_string(), TellurType::Bool);

    let left_not = c.add_node(NotNode {}, Placement { x: 0.0, y: 0.0 });
    let right_not = c.add_node(NotNode {}, Placement { x: 0.0, y: 0.0 });
    let and = c.add_node(AndNode {}, Placement { x: 0.0, y: 0.0 });
    let not = c.add_node(NotNode {}, Placement { x: 0.0, y: 0.0 });

    c.add_edge(Edge {
        from: (ComponentId::Input, "left".to_string()),
        to: (left_not.into(), "value".to_string()),
    });

    c.add_edge(Edge {
        from: (ComponentId::Input, "right".to_string()),
        to: (right_not.into(), "value".to_string()),
    });

    c.add_edge(Edge {
        from: (left_not.into(), "result".to_string()),
        to: (and.into(), "left".to_string()),
    });

    c.add_edge(Edge {
        from: (right_not.into(), "result".to_string()),
        to: (and.into(), "right".to_string()),
    });

    c.add_edge(Edge {
        from: (and.into(), "result".to_string()),
        to: (not.into(), "value".to_string()),
    });

    c.add_edge(Edge {
        from: (not.into(), "result".to_string()),
        to: (ComponentId::Output, "result".to_string()),
    });

    assert_matches!(c.evaluate(BTreeMap::from([
        ("left".to_string(), TellurTypedValue::Bool(true)),
        ("right".to_string(), TellurTypedValue::Bool(true)),
    ])), Ok(map) => {
        assert_matches!(map.get("result"), Some(TellurTypedValue::Bool(true)));
    });
    assert_matches!(c.evaluate(BTreeMap::from([
        ("left".to_string(), TellurTypedValue::Bool(true)),
        ("right".to_string(), TellurTypedValue::Bool(false)),
    ])), Ok(map) => {
        assert_matches!(map.get("result"), Some(TellurTypedValue::Bool(true)));
    });
    assert_matches!(c.evaluate(BTreeMap::from([
        ("left".to_string(), TellurTypedValue::Bool(false)),
        ("right".to_string(), TellurTypedValue::Bool(true)),
    ])), Ok(map) => {
        assert_matches!(map.get("result"), Some(TellurTypedValue::Bool(true)));
    });
    assert_matches!(c.evaluate(BTreeMap::from([
        ("left".to_string(), TellurTypedValue::Bool(false)),
        ("right".to_string(), TellurTypedValue::Bool(false)),
    ])), Ok(map) => {
        assert_matches!(map.get("result"), Some(TellurTypedValue::Bool(false)));
    });
}

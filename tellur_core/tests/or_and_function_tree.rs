use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use tellur_core::node::{TellurNode, TellurNodePlanned};
use tellur_core::tellur_std_node::logical::and::AndNode;
use tellur_core::tellur_std_node::logical::not::NotNode;
use tellur_core::tree::{NodeId, TellurNodeTree, TreeInput};
use tellur_core::types::{TellurRefType, TellurType, TellurTypedValue, TellurTypedValueContainer};

fn construct_or_tree() -> TellurNodeTree {
    TellurNodeTree {
        name: "or".to_string(),
        parameters: {
            let mut parameters = BTreeMap::new();
            parameters.insert(
                "left".to_string(),
                (TellurRefType::Immutable, TellurType::Bool),
            );
            parameters.insert(
                "right".to_string(),
                (TellurRefType::Immutable, TellurType::Bool),
            );
            parameters
        },
        returns: {
            let mut returns = BTreeMap::new();
            returns.insert("result".to_string(), TellurType::Bool);
            returns
        },
        nodes: {
            let mut nodes: BTreeMap<NodeId, (BTreeMap<String, TreeInput>, Box<dyn TellurNode>)> =
                BTreeMap::new();
            nodes.insert(
                NodeId(0),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "value".to_string(),
                            TreeInput::Parameter {
                                name: "left".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(NotNode {}),
                ),
            );
            nodes.insert(
                NodeId(1),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "value".to_string(),
                            TreeInput::Parameter {
                                name: "right".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(NotNode {}),
                ),
            );
            nodes.insert(
                NodeId(2),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "left".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(0),
                                output_name: "result".to_string(),
                            },
                        );
                        inputs.insert(
                            "right".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(1),
                                output_name: "result".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(AndNode {}),
                ),
            );
            nodes.insert(
                NodeId(3),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "value".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(2),
                                output_name: "result".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(NotNode {}),
                ),
            );
            nodes
        },
        outputs: {
            let mut outputs = BTreeMap::new();
            outputs.insert("result".to_string(), (NodeId(3), "result".to_string()));
            outputs
        },
    }
}

fn construct_and_tree() -> TellurNodeTree {
    TellurNodeTree {
        name: "and".to_string(),
        parameters: {
            let mut parameters = BTreeMap::new();
            parameters.insert(
                "left".to_string(),
                (TellurRefType::Immutable, TellurType::Bool),
            );
            parameters.insert(
                "right".to_string(),
                (TellurRefType::Immutable, TellurType::Bool),
            );
            parameters
        },
        returns: {
            let mut returns = BTreeMap::new();
            returns.insert("result".to_string(), TellurType::Bool);
            returns
        },
        nodes: {
            let mut nodes: BTreeMap<NodeId, (BTreeMap<String, TreeInput>, Box<dyn TellurNode>)> =
                BTreeMap::new();
            nodes.insert(
                NodeId(0),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "value".to_string(),
                            TreeInput::Parameter {
                                name: "left".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(NotNode {}),
                ),
            );
            nodes.insert(
                NodeId(1),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "value".to_string(),
                            TreeInput::Parameter {
                                name: "right".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(NotNode {}),
                ),
            );
            nodes.insert(
                NodeId(2),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "left".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(0),
                                output_name: "result".to_string(),
                            },
                        );
                        inputs.insert(
                            "right".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(1),
                                output_name: "result".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(construct_or_tree()),
                ),
            );
            nodes.insert(
                NodeId(3),
                (
                    {
                        let mut inputs = BTreeMap::new();
                        inputs.insert(
                            "value".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(2),
                                output_name: "result".to_string(),
                            },
                        );
                        inputs
                    },
                    Box::new(NotNode {}),
                ),
            );
            nodes
        },
        outputs: {
            let mut outputs = BTreeMap::new();
            outputs.insert("result".to_string(), (NodeId(3), "result".to_string()));
            outputs
        },
    }
}

fn calc(left: bool, right: bool, plan: &dyn TellurNodePlanned) -> bool {
    let result = plan.evaluate(vec![
        TellurTypedValueContainer::new(TellurTypedValue::Bool(left).into()),
        TellurTypedValueContainer::new(TellurTypedValue::Bool(right).into()),
    ]);
    match *result.unwrap()[0].try_read().unwrap() {
        TellurTypedValue::Bool(b) => b,
        _ => panic!(),
    }
}

#[test]
fn or_tree_should_work() {
    let plan = construct_or_tree().planned();
    assert_eq!(calc(true, true, &*plan), true);
    assert_eq!(calc(true, false, &*plan), true);
    assert_eq!(calc(false, true, &*plan), true);
    assert_eq!(calc(false, false, &*plan), false);
}

#[test]
fn and_tree_should_work() {
    let plan = construct_and_tree().planned();
    assert_eq!(calc(true, true, &*plan), true);
    assert_eq!(calc(true, false, &*plan), false);
    assert_eq!(calc(false, true, &*plan), false);
    assert_eq!(calc(false, false, &*plan), false);
}

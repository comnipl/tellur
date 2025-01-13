use std::collections::BTreeMap;

use tellur_core::node::TellurNode;
use tellur_core::tellur_std_node::logical::and::AndNode;
use tellur_core::tellur_std_node::logical::not::NotNode;
use tellur_core::tree::{NodeId, TellurNodeTree, TreeInput};
use tellur_core::types::{TellurRefType, TellurType};

pub fn or_with_andnot_tree() -> TellurNodeTree {
    TellurNodeTree {
        name: "or".to_string(),
        parameters: BTreeMap::from([
            (
                "left".to_string(),
                (TellurRefType::Immutable, TellurType::Bool),
            ),
            (
                "right".to_string(),
                (TellurRefType::Immutable, TellurType::Bool),
            ),
        ]),
        returns: BTreeMap::from([("result".to_string(), TellurType::Bool)]),
        nodes: BTreeMap::from([
            (
                NodeId(0),
                (
                    BTreeMap::from([(
                        "value".to_string(),
                        TreeInput::Parameter {
                            name: "left".to_string(),
                        },
                    )]),
                    Box::new(NotNode {}) as Box<dyn TellurNode>,
                ),
            ),
            (
                NodeId(1),
                (
                    BTreeMap::from([(
                        "value".to_string(),
                        TreeInput::Parameter {
                            name: "right".to_string(),
                        },
                    )]),
                    Box::new(NotNode {}) as Box<dyn TellurNode>,
                ),
            ),
            (
                NodeId(2),
                (
                    BTreeMap::from([
                        (
                            "left".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(0),
                                output_name: "result".to_string(),
                            },
                        ),
                        (
                            "right".to_string(),
                            TreeInput::NodeOutput {
                                id: NodeId(1),
                                output_name: "result".to_string(),
                            },
                        ),
                    ]),
                    Box::new(AndNode {}) as Box<dyn TellurNode>,
                ),
            ),
            (
                NodeId(3),
                (
                    BTreeMap::from([(
                        "value".to_string(),
                        TreeInput::NodeOutput {
                            id: NodeId(2),
                            output_name: "result".to_string(),
                        },
                    )]),
                    Box::new(NotNode {}) as Box<dyn TellurNode>,
                ),
            ),
        ]),
        outputs: BTreeMap::from([(
            "result".to_string(),
            TreeInput::NodeOutput {
                id: NodeId(3),
                output_name: "result".to_string(),
            },
        )]),
    }
}

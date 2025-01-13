use std::collections::BTreeMap;

use tellur_core::node::TellurNode;
use tellur_core::tellur_std_node::logical::not::NotNode;
use tellur_core::tree::{NodeId, TellurNodeTree, TreeInput};
use tellur_core::types::{TellurRefType, TellurType};

pub fn wrapped_not_tree() -> TellurNodeTree {
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

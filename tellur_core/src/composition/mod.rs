use core::panic;
use std::collections::{BTreeMap, BTreeSet};
use std::iter;

use crate::node::TellurNode;
use crate::tree::{NodeId, TellurNodeTree, TreeInput};
use crate::types::{TellurRefType, TellurType, TellurTypedValue};

pub struct Placement {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentId {
    Input,
    Node(NodeId),
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompositionEdge {
    pub from: ComponentId,
    pub from_output: String,
    pub to: ComponentId,
    pub to_input: String,
}

pub struct TellurComposition {
    tree: TellurNodeTree,
    placements: BTreeMap<ComponentId, Placement>,
    edges: BTreeSet<CompositionEdge>,
}

impl TellurComposition {
    pub fn new(name: String) -> Self {
        Self {
            tree: TellurNodeTree {
                name,
                parameters: BTreeMap::new(),
                returns: BTreeMap::new(),
                nodes: BTreeMap::new(),
                outputs: BTreeMap::new(),
            },
            placements: BTreeMap::new(),
            edges: BTreeSet::new(),
        }
    }

    pub fn clean_parameters(&mut self) {
        let edges = &mut self.edges;
        let mut edges_to_remove = Vec::new();
        for (nodeid, (inputs, boxed)) in self.tree.nodes.iter_mut() {
            let parameters = boxed.parameters();
            let parameter_keys: BTreeSet<String> = parameters.keys().cloned().collect();
            let input_keys: BTreeSet<String> = inputs.keys().cloned().collect();

            parameter_keys
                .difference(&input_keys)
                .filter(|&key| parameters[key].0 == TellurRefType::Immutable)
                .for_each(|key| {
                    inputs.insert(
                        (*key).to_string(),
                        TreeInput::Fixed {
                            value: parameters[key].1.default_value(),
                        },
                    );
                });

            input_keys
                .difference(&parameter_keys)
                .filter_map(|key| match inputs[key] {
                    TreeInput::Fixed { .. } => {
                        inputs.remove(key);
                        None
                    }
                    TreeInput::Parameter { ref name } => Some(
                        edges
                            .iter()
                            .filter(|&e| {
                                e.from == ComponentId::Input
                                    && e.from_output == *name
                                    && e.to == ComponentId::Node(*nodeid)
                                    && e.to_input == *key
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                            .into_iter(),
                    ),
                    TreeInput::NodeOutput {
                        id,
                        ref output_name,
                    } => Some(
                        edges
                            .iter()
                            .filter(|&e| {
                                e.from == ComponentId::Node(id)
                                    && e.from_output == *output_name
                                    && e.to == ComponentId::Node(*nodeid)
                                    && e.to_input == *key
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                            .into_iter(),
                    ),
                })
                .flatten()
                .for_each(|e| edges_to_remove.push(e));
        }
        edges_to_remove.iter().for_each(|e| self.remove_edge(e));
    }

    pub fn set_value(&mut self, id: &ComponentId, key: String, value: TellurTypedValue) {
        match id {
            ComponentId::Input => {
                panic!("Cannot set value for input");
            }
            ComponentId::Node(id) => {
                let (inputs, _) = self.tree.nodes.get_mut(id).unwrap();
                match inputs.get_mut(&key) {
                    Some(TreeInput::Fixed { .. }) | None => {
                        *inputs.get_mut(&key).unwrap() = TreeInput::Fixed { value };
                    }
                    _ => {
                        panic!("Disconnect node before setting value");
                    }
                }
            }
            ComponentId::Output => match self.tree.outputs.get_mut(&key) {
                Some(TreeInput::Fixed { .. }) | None => {
                    *self.tree.outputs.get_mut(&key).unwrap() = TreeInput::Fixed { value };
                }
                _ => {
                    panic!("Disconnect output before setting value");
                }
            },
        }
    }

    pub fn add_node(&mut self, node: impl TellurNode, placements: Placement) -> NodeId {
        let id = NodeId(
            (0..u32::MAX)
                .position(|i| !self.tree.nodes.contains_key(&NodeId(i)))
                .unwrap() as u32,
        );
        self.tree
            .nodes
            .insert(id, (BTreeMap::new(), Box::new(node)));
        self.placements.insert(ComponentId::Node(id), placements);
        self.clean_parameters();
        id
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.tree.nodes.remove(&id);
        self.placements.remove(&ComponentId::Node(id));
        self.edges
            .iter()
            .filter(|edge| edge.from == ComponentId::Node(id) || edge.to == ComponentId::Node(id))
            .cloned()
            .collect::<Vec<_>>()
            .iter()
            .for_each(|e| self.remove_edge(e));
        self.clean_parameters();
    }

    pub fn edges(&self) -> &BTreeSet<CompositionEdge> {
        &self.edges
    }

    fn to_tree_input(id: ComponentId, output: String) -> TreeInput {
        match id {
            ComponentId::Input => TreeInput::Parameter { name: output },
            ComponentId::Output => panic!("Output cannot be a source"),
            ComponentId::Node(id) => TreeInput::NodeOutput {
                id,
                output_name: output,
            },
        }
    }

    pub fn add_edge(&mut self, edge: CompositionEdge) {
        let adding_edge = edge.clone();
        let (from, from_output, to, to_input) =
            (edge.from, edge.from_output, edge.to, edge.to_input);
        let input = Self::to_tree_input(from, from_output);
        match to {
            ComponentId::Node(id) => {
                self.tree
                    .nodes
                    .get_mut(&id)
                    .unwrap()
                    .0
                    .insert(to_input.clone(), input);
            }
            ComponentId::Output => {
                self.tree.outputs.insert(to_input, input);
            }
            ComponentId::Input => panic!("Input cannot be a destination"),
        }
        self.edges.insert(adding_edge);
        self.clean_parameters();
    }

    pub fn remove_edge(&mut self, edge: &CompositionEdge) {
        match edge.to {
            ComponentId::Node(id) => {
                self.tree
                    .nodes
                    .get_mut(&id)
                    .unwrap()
                    .0
                    .remove(&edge.to_input);
            }
            ComponentId::Output => {
                self.tree.outputs.remove(&edge.to_input);
            }
            ComponentId::Input => panic!("Input cannot be a destination"),
        }
        self.edges.remove(edge);
        self.clean_parameters();
    }

    pub fn parameters(&self) -> &BTreeMap<String, (TellurRefType, TellurType)> {
        &self.tree.parameters
    }

    pub fn mut_parameters(&mut self) -> &mut BTreeMap<String, (TellurRefType, TellurType)> {
        &mut self.tree.parameters
    }

    pub fn returns(&self) -> &BTreeMap<String, TellurType> {
        &self.tree.returns
    }

    pub fn mut_returns(&mut self) -> &mut BTreeMap<String, TellurType> {
        &mut self.tree.returns
    }
}

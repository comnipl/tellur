use core::panic;
use std::collections::{BTreeMap, BTreeSet};

use crate::exception::TellurException;
use crate::node::TellurNode;
use crate::tree::{NodeId, TellurNodeTree, TreeInput};
use crate::types::{TellurRefType, TellurType, TellurTypedValue, TellurTypedValueContainer};

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone)]
pub struct NodeView {
    pub placement: Placement,
    pub parameters: BTreeMap<String, (TellurRefType, TellurType, InputView)>,
    pub returns: BTreeMap<String, TellurType>,
}

#[derive(Debug, Clone)]
pub enum InputView {
    ComponentOutput(ComponentId, String),
    Fixed(TellurTypedValue),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComponentId {
    Input,
    Node(NodeId),
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: (ComponentId, String),
    pub to: (ComponentId, String),
}

pub struct TellurComposition {
    tree: TellurNodeTree,
    placements: BTreeMap<ComponentId, Placement>,
    edges: BTreeSet<Edge>,
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
                                e.from.0 == ComponentId::Input
                                    && e.from.1 == *name
                                    && e.to.0 == ComponentId::Node(*nodeid)
                                    && e.to.1 == *key
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
                                e.from.0 == ComponentId::Node(id)
                                    && e.from.1 == *output_name
                                    && e.to.0 == ComponentId::Node(*nodeid)
                                    && e.to.1 == *key
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
            .filter(|edge| {
                edge.from.0 == ComponentId::Node(id) || edge.to.0 == ComponentId::Node(id)
            })
            .cloned()
            .collect::<Vec<_>>()
            .iter()
            .for_each(|e| self.remove_edge(e));
        self.clean_parameters();
    }

    pub fn nodes(&self) -> BTreeMap<NodeId, NodeView> {
        self.placements
            .iter()
            .filter_map(|(k, v)| match k {
                ComponentId::Node(id) => Some((*id, v)),
                _ => None,
            })
            .map(|(k, v)| {
                (
                    k,
                    NodeView {
                        placement: v.clone(),
                        parameters: self.tree.nodes[&k]
                            .0
                            .iter()
                            .zip(self.tree.nodes[&k].1.parameters().iter())
                            .map(|((k, v), (_, t))| {
                                (
                                    k.clone(),
                                    (
                                        t.0.clone(),
                                        t.1.clone(),
                                        match v {
                                            TreeInput::Parameter { name } => {
                                                InputView::ComponentOutput(
                                                    ComponentId::Input,
                                                    name.clone(),
                                                )
                                            }
                                            TreeInput::NodeOutput { id, output_name } => {
                                                InputView::ComponentOutput(
                                                    ComponentId::Node(*id),
                                                    output_name.clone(),
                                                )
                                            }
                                            TreeInput::Fixed { value } => {
                                                InputView::Fixed(value.clone())
                                            }
                                        },
                                    ),
                                )
                            })
                            .collect(),
                        returns: self.tree.nodes[&k].1.returns().clone(),
                    },
                )
            })
            .collect()
    }

    pub fn components(&self) -> BTreeSet<ComponentId> {
        self.placements.keys().cloned().collect()
    }

    pub fn evaluate(
        &self,
        inputs: BTreeMap<String, TellurTypedValue>,
    ) -> Result<BTreeMap<String, TellurTypedValue>, TellurException> {
        self.tree
            .planned()
            .evaluate(
                inputs
                    .into_values()
                    .map(|v| TellurTypedValueContainer::new(v.into()))
                    .collect(),
            )
            .map(|r| {
                r.into_iter()
                    .zip(self.tree.returns().keys())
                    .map(|(v, k)| (k.clone(), v.try_read().unwrap().clone()))
                    .collect()
            })
    }

    pub fn edges(&self) -> &BTreeSet<Edge> {
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

    pub fn add_edge(&mut self, edge: Edge) {
        let adding_edge = edge.clone();
        let (from, from_output, to, to_input) = (edge.from.0, edge.from.1, edge.to.0, edge.to.1);
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

    pub fn remove_edge(&mut self, edge: &Edge) {
        match edge.to.0 {
            ComponentId::Node(id) => {
                self.tree.nodes.get_mut(&id).unwrap().0.remove(&edge.to.1);
            }
            ComponentId::Output => {
                self.tree.outputs.remove(&edge.to.1);
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

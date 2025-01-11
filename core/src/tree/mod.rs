use std::collections::{BTreeMap, VecDeque};

use crate::exception::TellurException;
use crate::node::{TellurNode, TellurNodePlanned};
use crate::types::{TellurRefType, TellurType, TellurTypedValueContainer};

enum Input {
    Parameter { name: String },
    NodeOutput { id: NodeId, output_name: String },
}

enum PlannedInput {
    Parameter(usize),
    NodeOutput(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NodeId(u32);

pub struct TellurNodeTree {
    name: String,
    parameters: BTreeMap<String, (TellurRefType, TellurType)>,
    returns: BTreeMap<String, TellurType>,
    nodes: BTreeMap<NodeId, (BTreeMap<String, Input>, Box<dyn TellurNode>)>,
    outputs: BTreeMap<String, (NodeId, String)>,
}

pub struct TellurNodeTreePlanned {
    nodes: Vec<(Vec<PlannedInput>, Box<dyn TellurNodePlanned>)>,
    outputs: Vec<(usize, usize)>,
}

impl TellurNode for TellurNodeTree {
    fn ident(&self) -> &str {
        &self.name
    }

    fn parameters(&self) -> &BTreeMap<String, (TellurRefType, TellurType)> {
        &self.parameters
    }

    fn returns(&self) -> &BTreeMap<String, TellurType> {
        &self.returns
    }

    // TODO: 将来的にはここでメモリの配置まで決定
    fn planned(&self) -> Box<dyn TellurNodePlanned> {
        // TODO: サイクルを検出してエラーにする
        // TODO: 複数可変参照を取得されている場合にエラーにする
        // TODO: 枝刈りを行う

        let nodes_map: BTreeMap<NodeId, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, (id, _))| (*id, idx))
            .collect();

        let planned_nodes = self
            .nodes
            .values()
            .map(|(params, node)| {
                let p = node
                    .parameters()
                    .iter()
                    // TODO: 使われていないパラメーターを検出してエラーにする
                    .map(|(name, (_ref_type, _t))| {
                        match params.get(name) {
                            // TODO: 内部エラー (パラメーターが足りない)
                            Some(Input::Parameter { name }) => PlannedInput::Parameter(
                                self.parameters.keys().position(|k| k == name).unwrap(),
                            ),
                            // TODO: 内部エラー (ノードの出力が足りない)
                            Some(Input::NodeOutput { id, output_name }) => {
                                PlannedInput::NodeOutput(
                                    nodes_map[id],
                                    self.nodes[id]
                                        .1
                                        .returns()
                                        .keys()
                                        .position(|k| k == output_name)
                                        .unwrap(),
                                )
                            }
                            // TODO: パラメーターに対応する入力がないよ,というエラー
                            None => panic!(),
                        }
                        // TODO: ここで型チェックを実施
                    })
                    .collect::<Vec<PlannedInput>>();
                (p, node.planned())
            })
            .collect();

        let planned_outputs = self
            .returns
            .keys()
            .map(|name| {
                // TODO: ここで型チェックを実施
                let (id, output_name) = self.outputs.get(name).unwrap();
                (
                    nodes_map[id],
                    self.nodes[id]
                        .1
                        .returns()
                        .keys()
                        .position(|k| k == output_name)
                        .unwrap(),
                )
            })
            .collect();

        Box::new(TellurNodeTreePlanned {
            nodes: planned_nodes,
            outputs: planned_outputs,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
enum State {
    Waiting,
    Ready,
    Running,
    Finished,
}

// TODO: plannedの前と後でトレイトを分離

impl TellurNodePlanned for TellurNodeTreePlanned {
    fn evaluate(
        &self,
        args: Vec<TellurTypedValueContainer>,
    ) -> Result<Vec<TellurTypedValueContainer>, TellurException> {
        let mut memory: BTreeMap<(usize, usize), TellurTypedValueContainer> = BTreeMap::new();
        let mut state = vec![State::Waiting; self.nodes.len()];
        let mut executable: VecDeque<usize> = VecDeque::new();
        loop {
            if executable.is_empty() {
                for (idx, (p, _)) in self.nodes.iter().enumerate() {
                    if state[idx] != State::Waiting {
                        continue;
                    }
                    if p.iter().all(|p| match p {
                        PlannedInput::Parameter(_) => true,
                        PlannedInput::NodeOutput(n, _) => state[*n] == State::Finished,
                    }) {
                        state[idx] = State::Ready;
                        executable.push_back(idx);
                    }
                }
            }
            if self
                .outputs
                .iter()
                .all(|(n, _)| state[*n] == State::Finished)
            {
                return Ok(self
                    .outputs
                    .iter()
                    .map(|(n, o)| memory.get(&(*n, *o)).unwrap().clone())
                    .collect());
            } else if executable.is_empty() {
                panic!("No evaluatable nodes remain but outputs are not ready");
            }

            let executing = executable.pop_front().unwrap();
            let (p, n) = &self.nodes[executing];

            state[executing] = State::Running;
            let result = n.evaluate(
                p.iter()
                    .map(|p| match p {
                        PlannedInput::Parameter(i) => args[*i].clone(),
                        PlannedInput::NodeOutput(n, o) => memory.get(&(*n, *o)).unwrap().clone(),
                    })
                    .collect(),
            )?;

            for (i, r) in result.iter().enumerate() {
                memory.insert((executing, i), r.clone());
            }

            state[executing] = State::Finished;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tellur_std_node::logical::and::AndNode;
    use crate::tellur_std_node::logical::not::NotNode;
    use crate::types::TellurTypedValue;
    use pretty_assertions::assert_eq;

    use super::*;

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
                let mut nodes: BTreeMap<NodeId, (BTreeMap<String, Input>, Box<dyn TellurNode>)> =
                    BTreeMap::new();
                nodes.insert(
                    NodeId(0),
                    (
                        {
                            let mut inputs = BTreeMap::new();
                            inputs.insert(
                                "value".to_string(),
                                Input::Parameter {
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
                                Input::Parameter {
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
                                Input::NodeOutput {
                                    id: NodeId(0),
                                    output_name: "result".to_string(),
                                },
                            );
                            inputs.insert(
                                "right".to_string(),
                                Input::NodeOutput {
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
                                Input::NodeOutput {
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
                let mut nodes: BTreeMap<NodeId, (BTreeMap<String, Input>, Box<dyn TellurNode>)> =
                    BTreeMap::new();
                nodes.insert(
                    NodeId(0),
                    (
                        {
                            let mut inputs = BTreeMap::new();
                            inputs.insert(
                                "value".to_string(),
                                Input::Parameter {
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
                                Input::Parameter {
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
                                Input::NodeOutput {
                                    id: NodeId(0),
                                    output_name: "result".to_string(),
                                },
                            );
                            inputs.insert(
                                "right".to_string(),
                                Input::NodeOutput {
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
                                Input::NodeOutput {
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
}

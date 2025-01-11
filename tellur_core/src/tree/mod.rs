mod plan;

use std::collections::BTreeMap;

use crate::node::{TellurNode, TellurNodePlanned};
use crate::types::{TellurRefType, TellurType};

use self::plan::{PlannedInput, TellurNodeTreePlanned};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(pub u32);

pub enum TreeInput {
    Parameter { name: String },
    NodeOutput { id: NodeId, output_name: String },
}

pub struct TellurNodeTree {
    pub name: String,
    pub parameters: BTreeMap<String, (TellurRefType, TellurType)>,
    pub returns: BTreeMap<String, TellurType>,
    pub nodes: BTreeMap<NodeId, (BTreeMap<String, TreeInput>, Box<dyn TellurNode>)>,
    pub outputs: BTreeMap<String, (NodeId, String)>,
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
                            Some(TreeInput::Parameter { name }) => PlannedInput::Parameter(
                                self.parameters.keys().position(|k| k == name).unwrap(),
                            ),
                            // TODO: 内部エラー (ノードの出力が足りない)
                            Some(TreeInput::NodeOutput { id, output_name }) => {
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

        Box::new(TellurNodeTreePlanned::new(planned_nodes, planned_outputs))
    }
}

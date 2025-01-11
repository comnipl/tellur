use std::collections::{BTreeMap, VecDeque};

use crate::exception::TellurException;
use crate::node::TellurNodePlanned;
use crate::types::TellurTypedValueContainer;

pub(super) struct TellurNodeTreePlanned {
    nodes: Vec<(Vec<PlannedInput>, Box<dyn TellurNodePlanned>)>,
    outputs: Vec<(usize, usize)>,
}
pub(super) enum PlannedInput {
    Parameter(usize),
    NodeOutput(usize, usize),
}

#[derive(Clone, PartialEq, Eq)]
enum State {
    Waiting,
    Ready,
    Running,
    Finished,
}

impl TellurNodeTreePlanned {
    pub(super) fn new(
        nodes: Vec<(Vec<PlannedInput>, Box<dyn TellurNodePlanned>)>,
        outputs: Vec<(usize, usize)>,
    ) -> TellurNodeTreePlanned {
        TellurNodeTreePlanned { nodes, outputs }
    }
}

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

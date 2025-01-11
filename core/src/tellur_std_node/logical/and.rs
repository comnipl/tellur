use std::sync::LazyLock;

use crate::exception::TellurException;
use crate::node::{TellurNode, TellurNodePlanned, TellurParameters, TellurReturns};
use crate::types::{TellurRefType, TellurType, TellurTypedValue, TellurTypedValueContainer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndNode {}
struct AndNodePlanned {}

const IDENT: &str = "and";

static PARAMETERS: LazyLock<TellurParameters> = LazyLock::new(|| {
    let mut parameters = TellurParameters::new();
    parameters.insert(
        "left".to_string(),
        (TellurRefType::Immutable, TellurType::Bool),
    );
    parameters.insert(
        "right".to_string(),
        (TellurRefType::Immutable, TellurType::Bool),
    );
    parameters
});

static RETURNS: LazyLock<crate::node::TellurReturns> = LazyLock::new(|| {
    let mut returns = crate::node::TellurReturns::new();
    returns.insert("result".to_string(), TellurType::Bool);
    returns
});

impl TellurNode for AndNode {
    fn ident(&self) -> &str {
        IDENT
    }

    fn parameters(&self) -> &TellurParameters {
        &PARAMETERS
    }

    fn returns(&self) -> &TellurReturns {
        &RETURNS
    }
    fn planned(&self) -> Box<dyn TellurNodePlanned> {
        Box::new(AndNodePlanned {})
    }
}

impl TellurNodePlanned for AndNodePlanned {
    fn evaluate(
        &self,
        args: Vec<TellurTypedValueContainer>,
    ) -> Result<Vec<TellurTypedValueContainer>, TellurException> {
        let [left, right] = args.as_slice() else {
            panic!()
        };
        let TellurTypedValue::Bool(left) = *left.try_read().unwrap() else {
            panic!()
        };
        let TellurTypedValue::Bool(right) = *right.try_read().unwrap() else {
            panic!()
        };
        Ok(vec![TellurTypedValueContainer::new(
            TellurTypedValue::Bool(left && right).into(),
        )])
    }
}

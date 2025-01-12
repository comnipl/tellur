use std::sync::LazyLock;

use crate::exception::TellurException;
use crate::node::{TellurNode, TellurNodePlanned, TellurParameters, TellurReturns};
use crate::types::{TellurRefType, TellurType, TellurTypedValue, TellurTypedValueContainer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotNode {}
struct NotNodePlanned {}

const IDENT: &str = "not";
static PARAMETERS: LazyLock<TellurParameters> = LazyLock::new(|| {
    let mut parameters = TellurParameters::new();
    parameters.insert(
        "value".to_string(),
        (TellurRefType::Immutable, TellurType::Bool),
    );
    parameters
});

static RETURNS: LazyLock<crate::node::TellurReturns> = LazyLock::new(|| {
    let mut returns = crate::node::TellurReturns::new();
    returns.insert("result".to_string(), TellurType::Bool);
    returns
});

impl TellurNode for NotNode {
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
        Box::new(NotNodePlanned {})
    }
}

impl TellurNodePlanned for NotNodePlanned {
    fn evaluate(
        &self,
        args: Vec<TellurTypedValueContainer>,
    ) -> Result<Vec<TellurTypedValueContainer>, TellurException> {
        let [value] = args.as_slice() else { panic!() };
        let TellurTypedValue::Bool(value) = *value.try_read().unwrap() else {
            panic!()
        };
        Ok(vec![TellurTypedValueContainer::new(
            TellurTypedValue::Bool(!value).into(),
        )])
    }
}

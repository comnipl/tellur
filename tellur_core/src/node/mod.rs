use std::collections::{BTreeMap, BTreeSet};

use crate::exception::TellurException;
use crate::types::{TellurRefType, TellurType, TellurTypedValue, TellurTypedValueContainer};

pub type TellurParameters = BTreeMap<String, (TellurRefType, TellurType)>;
pub type TellurReturns = BTreeMap<String, TellurType>;

pub trait TellurNode: Send + Sync + 'static {
    fn ident(&self) -> &str;
    fn parameters(&self) -> &TellurParameters;
    fn returns(&self) -> &TellurReturns;
    fn planned(&self) -> Box<dyn TellurNodePlanned>;
}
pub trait TellurNodePlanned {
    fn evaluate(
        &self,
        args: Vec<TellurTypedValueContainer>,
    ) -> Result<Vec<TellurTypedValueContainer>, TellurException>;
}

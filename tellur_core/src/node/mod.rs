use std::collections::BTreeMap;
use std::fmt::Debug;

use crate::exception::TellurException;
use crate::types::{TellurRefType, TellurType, TellurTypedValueContainer};

pub type TellurParameters = BTreeMap<String, (TellurRefType, TellurType)>;
pub type TellurReturns = BTreeMap<String, TellurType>;

pub trait TellurNode: Debug + Send + Sync + 'static {
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
